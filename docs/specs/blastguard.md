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
  `>` / `>>` の書き込み先の語は改行をまたいで読まず（空白・タブのみ飛ばす、`skip_redirect_word_gap`）、
  行末の `>`（改行・テキスト末尾の直前、`redirect_word_missing_at_line_end`）は書き込み先として数えない
  （シェルでは何も開かない。backlog 0fccf1eb: `>` で終わる here-document 本文行の次の区切り行を
  `> EOF` と誤読していた）。
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
- **テスト fixture（git repo を含む）は workspace の `target/` 配下に作らない** — blastguard の integration
  test が作る fixture（とりわけ `git init` / `git worktree add` した repo）が `target/`（`CARGO_TARGET_TMPDIR`
  / `<workspace>/target`）に残ると、その worktree は無視されたビルド出力ディレクトリ内に入れ子の `.git`
  を持ち、forced な worktree 撤去が正しく Ask（subagent は Deny）になって撤去できなくなる
  （観測: 842 個の `.git`）。fixture base は `tests/support/neutral_base.rs` の `neutral_base()` で、
  既定の置き場は `$HOME/.blastguard-test-fixtures/blastguard-worktree-root-fixtures/run-<pid>-<nanos>[-n]`
  （この suite 専用の scratch 名前空間。テストバイナリ実行ごとに `create_dir` で一意。
  `BLASTGUARD_TEST_FIXTURE_DIR` で上書き可、上書き時はそれが唯一の候補）。`/tmp`・`/private/tmp`・`/var/tmp`
  は temp safe root、`$TMPDIR`（macOS は `/private/var/folders/...`）は temp root かつ削除原則上の
  システムディレクトリなので使わない。使用前に実 hook で 2 本の較正（cwd 外の `rm -rf` が厳密に Deny、
  cwd 内の `rm -rf` が厳密に Ask）を通し、通らない場所は panic で拒否する（場所が判定を左右しないこと）。
  各実行はプロセス終了時（`atexit`）に**自分の** run ディレクトリだけを消す（`symlink_metadata` で実ディレクトリ
  かつ正準パスが作成時と一致する場合のみ。失敗は stderr、panic しない、他は消さない）。開始時の prune は
  専用ディレクトリ直下の `run-*` のうち実ディレクトリ（symlink・非ディレクトリは skip）・24 時間無更新・
  正準パスが `<正準な専用ディレクトリ>/<名前>` に一致するものだけを消し、疑わしいものは skip する。
  `CARGO_TARGET_TMPDIR` に直接 fixture を繋いで git repo を作っていたテスト（`backlog_audit_s02.rs`・
  `backlog_worktree_redirect_and_sq_fp_20261107.rs`）は `neutral_base()` に移した。git repo を作らない一部の
  テストは引き続き `CARGO_TARGET_TMPDIR` 配下に scratch を作る。`tests/fixtures_outside_target_20261114.rs`
  の RED（`#[ignore]`）は scan の再アンカー後に外す（backlog 37955e32）。

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
  `reset --hard`（例外 9352d929 / GH #1660: rev が文字どおり `HEAD`、top-level の単一 git コマンドで
  global option は `-C <dir>` 1 回まで、git が走るディレクトリが解決済みで `.git` が通常ファイル
  〈linked worktree checkout をディスクで観測〉、かつ `git status --porcelain=v1 -z --untracked-files=all
  --ignore-submodules=none` が exit 0・出力空、`git ls-files -v -z` が exit 0 で全 entry が `H`
  〈assume-unchanged / skip-worktree なし〉のときだけ allow。probe 失敗・timeout・非 0・それ以外の出力は
  deny のまま — `detect/git.rs` の `reset_hard_head_in_clean_linked_worktree`）、
  `checkout --force`/`-f`、`checkout -- .`（whole-tree）、`switch -f/--discard-changes`、
  `stash` の `-a/--all/-u`（追跡外・無視ファイルを working-tree から除去）、`git rm`（`--cached` 含む・
  保護パス target のみ）、および `checkout`/`restore` の operand が保護パスの場合。**通常のパスへの
  `git checkout [<rev>] -- <path>` は壊すもので判定する**（`detect/git.rs` の `checkout_pathspec_verdict`）:
  git probe（`reversible::probe_tree`）が path 以下に未コミット変更無しと答えれば Allow、有れば Deny、
  聞けなければ Ask。**disk に無い path**（probe_tree は Undetermined）は working tree に壊すものが無いので、
  `reversible::probe_absent_path` が `lstat` NotFound・最寄りの既存祖先が repo 内・index entry が `H` のみ・
  `git diff --cached --quiet -- <path>` exit 0（HEAD と異なる staged 版無し）を全て観測したときだけ Allow、
  exit 1（staged 版を上書きする）は Deny、それ以外（repo 外・git 失敗・`:` で始まる pathspec magic・
  `--*-pathspecs` global）は Ask（tests/checkout_absent_path_fp_20261108.rs）。`GIT_ICASE_PATHSPECS` 等の
  環境変数は見ていない（open）。日常操作を blanket deny しない）。**working tree を書き戻す `git restore`**
  （`--staged` 単独以外。managed worktree checkout 内は下記のとおり Allow）は**失うもので判定する**（ユーザー裁定
  2026-11-08、issue #1604、`detect/git.rs` の `restore_discard_verdict`）: 保護パス operand は先に Deny
  （content が commit 済みでも `--source` で古い無力化版を戻せるため）。それ以外は全 pathspec について
  `reversible::probe_restore_content` が「index blob と working tree の raw bytes
  （`git hash-object --no-filters`）の両方が、ref から到達可能な commit に有る」を観測したときだけ Allow。
  到達性は `git cat-file -e <id>` ＋ `git log --all -n1 --find-object=<id> -- <path>`（path 限定の
  commit walk。`rev-list --objects --all` は全 tree を歩くので採らない。2026-11-08 本 repo 6219 commit で
  0.3 s 実測）で見る。別 path にだけ commit された blob・merge の解決でだけ入った blob は見つからず
  Deny 側に倒れる。どれかが commit に無ければ Deny、判定不能（subcommand 前の global option・
  モデル外の option〈`--patch`・`--pathspec-from-file` 等〉・pathspec 無し・literal でない path・
  `:` magic・directory・untracked/absent・index entry が `H`/stage 0/通常 mode の 1 件でない・git 失敗・
  timeout・probe 無し）も Deny（tests/backlog_git_restore_content_in_commit_fp_20261107.rs）。
  file mode の差は content とみなさない、
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
  保護パス・`git stash clear`（`refs/stash` は全 worktree で共有され、clear は他セッションの stash も
  全部消す）は対象外のまま deny。`git stash drop [<stash>]` は 2026-11-08 のユーザー裁定
  （「drop のみ Allow」）で場所を問わず allow（`detect/git.rs` の `stash` arm）。
- **`core.hooksPath` の付け替えは Deny（`detect/git.rs` の `config` arm、`HOOKSPATH_REASON`）** —
  `git config` の引数に `core.hooksPath` が現れ、git の argv 文法で read と判定できるときだけ allow する
  （`config_argv_is_read`）。read は (1) read-mode flag（`--get`・`--get-all`・`--get-regexp`・`--get-urlmatch`・
  `--list`・`-l`）が**option prefix（最初の非 option 語より前）にある**（backlog e103cd56、blastguard 0.2.388）、
  (2) mode flag なしで positional が key 1 つだけ（`git config core.hooksPath`・`git config --local core.hooksPath`）、
  (3) 最初の positional が `get`（`git config get core.hooksPath`）のいずれか（(2)(3) はユーザー裁定 2026-11-08、
  backlog 8fe3a5c3）。git は最初の非 option 語で option 解析を止めるので、key の後ろの `--get` 等は書き込まれる
  値であり（`git config core.hooksPath --get` は値 `--get` を書く。git 2.50.1 で観測）、read と数えない。
  positional が 2 つ以上（`K value`・`K ''`・`K --`・`set K v`・`unset K`）は write。prefix の write-mode flag
  （`--unset`・`--add` 等）や未知の option は read と認めない（Deny 側）。
  `git -c core.hooksPath=…`（global option の inline 設定）も Deny。
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
  `git rev-parse --show-toplevel` と `git status --porcelain=v1 -z --ignored=matching
  --untracked-files=all --ignore-submodules=none` がともに exit 0 で、出力が porcelain v1 として読め、
  全 entry が「regenerable な build 出力と確認できた ignored directory」のときだけ Allow（空も Allow）。
  build 出力と数えるのは、`!!` で、git が末尾 `/` 付き（directory）で出し、worktree root 相対の
  **最後の path component** が `deletion::BUILD_OUTPUT_NAMES`（rm の class 4 と同じ一覧: `target`・
  `node_modules`・`__pycache__` 等）のどれかに完全一致し、`.`/`..`/空 component を含まず、その entry と
  全ての祖先 prefix が今 symlink でない実 directory（`symlink_metadata`）であるものだけ。component 単位の
  一致で substring ではない（`target-secrets/`・`x/target.env` は数えない）。worktree 外を指す symlink の
  `target` は directory でないので数えない（Ask）。modified / staged / untracked、build 名でない ignored
  （`.env`・鍵・設定・未知の名前）は Ask（強制削除は directory ごと消すため）。backlog 2089dfc5、
  blastguard 0.2.537 で、clean な linked worktree に ignored `target/` しか無い場合の Ask/Deny（誤検知）を
  解消（`tests/worktree_remove_ignored_build_output_fp_20261114.rs`）。build 名の ignored directory でも、
  その中に **nested checkout（`.git` という名前の entry — file・directory・symlink を問わず — が任意の深さに
  1 つでもある）** があれば build output とみなさず Ask（subagent では Deny）: git は `target/` 内に
  `git worktree add` された linked worktree を `target/` 1 行としか報告せず、強制削除がその未 commit
  file ごと消すことを実観測した（backlog b997a3d5、blastguard 0.2.550、同 test の t3）。判定は directory を
  `harness_core::boundary::read_dir_entries_checked` で歩き、各 entry を `symlink_metadata` で分類する
  （symlink は辿らず leaf として扱う）。listing の失敗・途中 entry の失敗・walk 中の directory 消失・
  `symlink_metadata` 失敗、1 command あたり 1,000,000 entry 超過、または 3 秒の walk 期限超過は判定不能として
  Ask（Allow にしない）。期限は各 directory の listing の前と各 entry ごと（`symlink_metadata` の前）に
  確認する（blastguard 0.2.551）。期限は PreToolUse hook の 10 秒 timeout（kill された hook は tool を止めない）
  より十分手前で止めることを狙うが、**walk の壁時計時間に硬い上限は無い**: 実行中の 1 回の listing・1 回の
  `symlink_metadata` は中断できないので、巨大な directory や応答しない mount ではその 1 回分だけ期限を
  超えうる（その間に hook が 10 秒で kill されれば tool は止まらない — この budget では塞がない fail-open）。
  実測（2026-11-14、macOS/APFS、同 walk の `-O` replica）: 本 workspace の cargo `target/`
  55,883 entry を warm 585–694 ms・初回 1,809 ms、`target/debug` 21,791 entry を 179 ms（約 10–32 µs/entry）。
  この実測からの外挿では約 9 万（cold）〜30 万（warm）entry で期限に先に達して Ask になる（Allow にはならない）
  が、これは 1 回の測定からの外挿であり保証ではない。
  test fixture の repo（`.git` を持つ）を `target/tmp` 等に置いた target も nested checkout として Ask。
  walk するのは operand の worktree 内の directory だけで、下の裁定の範囲内。**検査するのは operand に名指しされた
  worktree だけ**（ユーザー裁定 2026-11-14「gateもconduktも絶対に勝手に他のworktreeを参照してはならない」、
  backlog b997a3d5、blastguard 0.2.546）: git を起動する前に、operand 自身が実 directory（symlink でない）で
  `<operand>/.git` が regular file（symlink でも directory でもない）＝ linked worktree の root であることを
  `symlink_metadata` だけで確かめ（親 directory は辿らず、`.git` file の `gitdir:` も読まない）、さらに
  `git -C <operand> rev-parse --show-toplevel` の結果と operand を両方 canonicalize して一致を要求する。
  それ以外（main worktree 内の ignored `target/` のように worktree root でない path、`.git` が directory の
  main worktree、symlink、存在しない path、stat 失敗、toplevel 不一致）は Ask で、
  包含する worktree や他の worktree の状態で判定しない。worktree remove の probe は `-C <operand>` だけで
  走り（`--git-dir`/`--work-tree` は転送しない）、累積 `-C` は相対 operand を置く起点にのみ使う。
  branch force-delete の probe は git が実際に作用する repo
  （累積 `-C`〈相対は segment の base 起点〉・`--git-dir`・`--work-tree`）で走らせ、相対 worktree path は
  その `-C` directory 起点で置く。判定不能 — operand 無し・base 不明（`cd "$VAR"` 後など）・置けない/
  未知の global option・default branch 解決不能・probe の想定外 exit / spawn 失敗 / timeout・
  絶対 path でない toplevel・porcelain v1 として読めない status 出力 — は全て Ask。
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
- **scratch tree は管理された場所にだけ作る（`detect/scratch_place.rs`、backlog ef7c9fbe、blastguard 0.2.375）** —
  `git clone` と `cp -r`（`-R`/`-a` を含む）の作成先が、`SafeRoots::classify_managed_place` の管理された場所
  （session scratchpad / temp roots・worktree storage roots・project）の外なら `Ask`。内側なら判定は変えない。
  `mkdir -p` は対象外（2026-11-07 ユーザー裁定）。未知の option・verb が取る数を超える operand・literal でない
  作成先・relative の基点不明・`..` はすべて `Ask`（判定不能は制限側）。この規則は `Allow` を `Ask` に上げるだけで、
  他の規則の `Deny`/`Ask` はそのまま。cp/mv/install/ln の option 解析は getopt 形式の共通 parser に一本化した
  （`-t <dir>` の clustered 形を含む）。library 入口 `detect::detect` にも適用する（GitHub #1637、blastguard
  0.2.434）。以前はこの入口（`SafeRoots::none`）だけ規則を飛ばして `Allow` を返し、無人で `sh -c` に渡す condukt
  `verify.rs`（`launch_and_reflux`/`launch_server_and_probe`/`launch_in_container`）・specguard `forge/implement.rs`・
  daily の呼び出しが hook より寛容な判定を得ていた（fail-open）。現在は、他の規則には `SafeRoots::none` のまま、
  この規則だけ**プロセス**の管理された場所（process cwd — これらの呼び出しの `sh -c` が走る場所 — ・
  `CLAUDE_PROJECT_DIR`・`HOME`・`TMPDIR`、hook と同じ `SafeRoots::new` の導出規則と実 path 解決）で判定する
  （`SafeRoots::with_managed_places`、`scratch_place::process_place_model`）。外側・判定不能は `Ask` で、呼び出し側は
  `.hardened()` で `Deny` に倒すので起動前に拒否される。location model が全く無い scope（裸の `SafeRoots::none` を
  `detect_scoped` に渡す）では作成先を示せないので、tree を作る `git clone`/`cp -r` はすべて `Ask`。何も作らない形
  （repository 無しの `git clone`、operand 1 個の `cp -r`）の判定は変わらない。
- **bash 3.2 の読みも判定する（`detect/readings.rs` の `detect_bash_every_reading`、blastguard 0.2.305）** —
  macOS の `/bin/bash` 3.2 と `/bin/sh` は `$(…)` 内の here-document 本文を command text として読むので、
  本文中の `)` が substitution を早く閉じ、続く text が実行されうる（2026-11-07 に harmless marker で観測:
  本文が外側の double quote も閉じる形 `hi)"; P; echo "`、および unquoted `$(…)`。zsh は実行しない）。
  Bash の top-level 判定は、従来の読み（modern）に加え、text が `<<` と `$(` または backtick を併せ持つとき
  だけ bash 3.2 の読み（`detect/substitution.rs` の `under_bash32_reading`）でも `detect_bash` を走らせ、
  **厳しい方**（`Deny > Ask > Allow`、`VerdictAcc`）を返す。bash 3.2 が別の位置で閉じると証明できればその
  parse を判定し、bash 3.2 が入力切れ（syntax error でその command 以降は実行されない）になると証明できれば
  modern の終端で再判定する。bash 3.2 の parse をモデル化できない場合は少なくとも Ask（判定不能を Allow に
  丸めない）。各 pass は満額の node budget から始める（D4）。**残余（未対応、RED を `#[ignore]` で固定）**:
  早期 close 後の text の読み違い（escape した quote、quote を含む `#` comment、`<(`。backlog 089bedd4）、
  zsh が unquoted `<<EOF` 本文を展開する形（backlog 5916d385）。
  bash 3.2 の読みで quote・backtick・backslash を含む comment 行（unquoted `$(` 内）は parse をモデル化
  できないので Ask のままだが、その comment が quote を隠すと仮定した**候補の読み**
  （`substitution::assuming_comment_hides_quotes`、`readings::bash32_comment_candidate`。D1-D3 で
  /bin/bash 3.2・/bin/sh が早期 close 後の command を実行することを観測）でも両 bash 3.2 pass を
  再判定し、そこで Deny なら Deny を返す（issue #1614、backlog d328ea88）。候補の読みは判定を足すだけで、
  Allow でも Ask は消えない。
- **shell の字句どおりの読みも判定する（`detect/shell_lex.rs` の `shell_lexed_reading`、backlog e2b9ffb1、
  blastguard 0.2.379）** — segment 分割器と line-level scan は quote を素朴に追うだけで `#` comment も
  backslash escape も知らないため、comment 中の quote（`echo $HOME #e"`）や escape した quote（`x\'a`）が
  shell の開かない quote を開き、**次の行**（`rm -rf /etc` 等）を quote 内の operand として隠していた
  （heredoc 無しで /bin/bash 3.2・/bin/sh・/bin/zsh すべてが次の行を実行することを観測）。
  `detect_bash_every_reading` は、comment を除去し（改行は残す）escape された `'`/`"`/`` ` `` を `_` に
  置き換えた text が元と異なるとき、それも `detect_bash` で判定し**厳しい方**を返す（読みを足すだけで
  判定を減らさない）。single quote・`$'…'`・double quote・backtick・`$(…)`/`<(…)`/`(…)` の入れ子・
  `$((…))`・here-document 本文（verbatim に写す）をモデル化する。あわせて unquoted の `<(…)`/`>(…)`
  process substitution の本体も `$(…)` と同じく実行される command として解析する
  （`executed_substitution_payloads`）。
- **行継続（`\`＋改行）を除いた読みも判定する（`detect/shell_lex.rs` の `line_continuation_reading`、
  backlog 040b959b）** — shell は単語分割の前に、unquoted・double quote 内・`$(…)`・
  backtick・`$((…))` の `\`＋改行を取り除く（single quote・`$'…'`・`#` comment・here-document 本文では
  取り除かない。comment 末尾の `\` は次の行を継続しない。/bin/bash 3.2・/bin/sh・/bin/zsh で観測）。
  segment 分割器と単語 scan はその改行で行を割るため、コマンド語・引数・`-c` operand を `\`＋改行で
  二分すると system-harm のコマンドが無害な 2 行に見えて Allow になっていた。`detect_bash_every_reading`
  は行継続だけを除いた text（comment・escape された quote はそのまま）が元と異なるとき、それも
  `detect_bash` で判定し**厳しい方**を返す。`analyze_shell_payload` も payload（`bash -c '…'` の
  single quote 内の行継続は内側の shell が取り除く）に同じ読みを足し、Deny > Ask > Allow で合成する
  （`readings::shell_payload_verdict`）。stdin から読む shell program（`bash`/`sh` への here-string、
  quoted / unquoted の here-document、`echo … | bash` の literal）も、読む shell が行継続を取り除くので、
  各 program text に行継続を除いた読みを足して同じ `interpreter_code_verdict` で判定する
  （`readings::shell_program_readings`、`detect/payload.rs`）。読む側が shell でない here-document
  （`cat <<'EOF' > file` の data 本文、python の stdin program）には足さない。読みが判定不能（行を join する here-document の未モデル形・入れ子上限）なら Ask。`shell_lexed_reading`
  も同じ位置の行継続を取り除き、`#` を comment 開始とみなす単語境界は綴り直した後の text で決める。
  テストは `tests/backlog_20261113_toplevel_line_continuation.rs`・
  `tests/backlog_20261113_bn_shell_stdin_payload.rs`・
  `tests/line_continuation_deny_and_controls_20261113.rs`（system-harm は main・subagent とも Deny、
  無害な行継続コマンドは Allow のまま）。
- **Python の非 raw 文字列 literal は escape を decode した値でも判定する（backlog 47b20647、
  `detect/interpreter/python_escape.rs` の `decode_non_raw`）** — Python は非 raw literal
  （`'…'`・`"…"`・三重引用・`b''`・`f''`）の中の `\`＋改行を取り除くので、`open('/etc/ho\<LF>sts','w')`
  は `/etc/hosts` に書く。literal 本文（改行を含むので候補 path から外れていた）だけを見ていたため Allow
  になっていた。Python lexer（`python_semantics::lex`）の非 raw 文字列の text を Python と同じ decode
  （`\`＋LF／CRLF の除去、`\\` `\'` `\"` `\a` `\b` `\f` `\n` `\r` `\t` `\v`、8 進、`\xHH`、str のみ
  `\uXXXX`/`\UXXXXXXXX`、それ以外の `\` 対はそのまま）にし、Python が読む layer-0 program
  （`python -c`、および stdin から読む `python3 -`・`python3 /dev/stdin`・pipe・here-document。
  `interpreter_code_verdict` の `PythonReader::InlineEval`/`Stdin`）では、その decode 値も literal 集合に
  **追加**して判定する（置き換えない）。unquoted here-document（`python3 - <<EOF`）は shell が本文の
  `\<LF>` を取り除き `\\` `\$` `` \` `` を 1 文字にしてから Python に渡すので、その読み
  （`readings::unquoted_here_document_reading`）も program text に足す。
  `\N{NAME}` は印字可能 ASCII 95 文字すべての名前（`LATIN SMALL|CAPITAL LETTER A..Z`・`DIGIT ZERO..NINE`
  は規則で、記号・空白は `ASCII_PUNCTUATION_NAMES` の表で。Python と同じく大文字小文字を区別せず完全一致、
  別名 `SP` も可。`python_escape::ascii_by_name`）を decode する（`/\N{LATIN SMALL LETTER E}tc/hosts` は
  `/etc/hosts` として main・subagent とも Deny）。
  **decode できない literal は既知の値として扱わない**: それ以外の `\N{NAME}`（非 ASCII・制御文字・存在しない名前。
  非 ASCII 名の表は持たない）は 1 文字の hole（`python_escape::Decoded::Holed`）とし、書き込みを持つ program では
  `detect/interpreter/python_literal_target.rs` の `unknown_literal_target_verdict` が hole を `/`・`.`・
  通常の 1 文字で埋めた全読み（hole 4 個まで）を system-directory 判定にかけ、どれかが system directory に
  着地すれば **Deny**（`/etc/\N{SNOWMAN}`）。hole が directory 名の中に
  あり（`/\N{SNOWMAN}tc/hosts`）どの読みも Deny でなければ **Ask**（subagent では Deny）、hole が 5 個以上も Ask。
  hole が最後の segment だけにあり directory が確定している場合（`/tmp/\N{SNOWMAN}`）は何も足さない。
  malformed な escape（`\x2`・`\N` の後に `{NAME}` が無い等、`Decoded::Undecodable`）は書き込みを持つ
  program で Ask。Python lexer の `Tok::Str` の text は、decode できない本文に
  ついては従来の読みのまま（Python FILE judge がこれを読む）。raw literal（`r'…'`、Python は `\` と改行を残す）
  は decode しない。テストは `tests/backlog_20261113_python_string_literal_line_continuation.rs`・
  `tests/backlog_20261113_python_named_escape_and_stdin_bn.rs`・
  `tests/python_literal_line_continuation_deny_20261113.rs`（system-harm 形は main・subagent とも Deny、
  directory 名中の非 ASCII `\N{…}` は main Ask／subagent Deny、`print('a\<LF>b')`・`open('/tmp/x\<LF>y','w')`・
  `print('\N{SNOWMAN}')`・`open('/tmp/\N{SNOWMAN}','w')`・無害な stdin program・raw 形は Allow のまま）。
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
  `--to-cmd`/`--cc-cmd`/`--header-cmd`/`--sendmail-cmd`、`ext::<command>` transport（`fetch`/`pull`/`push`/
  `clone`/`ls-remote`/`remote`/`config`/`archive`/`fetch-pack`/`send-pack`/`submodule` の引数、`-c` の key/値。
  `protocol.ext.allow` に関わらず）。command-valued key には `trailer.<x>.cmd`/`.command`・
  `core.alternateRefsCommand`・`imap.tunnel`・`sendemail.sendmailCmd`・`submodule.<x>.update=!…`
  （`!` 以外の値は組込み戦略なので対象外）・`core.fsmonitor`（bool 以外）も含む（backlog 3c750df2）。
  語は shell の quote を外してから見る（ANSI-C `$'…'` も decode する。global option 中の decode できない
  `$'…'` は ban）。**git の環境変数にも同じ ban を適用する**（backlog 3c750df2。`detect/executors.rs` の
  `env_exec_var_verdict`）: `GIT_PAGER`/`GIT_EDITOR`/`GIT_SEQUENCE_EDITOR`/`GIT_SSH_COMMAND`/`GIT_SSH`/
  `GIT_EXTERNAL_DIFF`/`GIT_ASKPASS`/`GIT_PROXY_COMMAND` は対応する config key の値として、
  `GIT_CONFIG_KEY_<n>`/`GIT_CONFIG_VALUE_<n>` と `GIT_CONFIG_PARAMETERS` の各 entry は `git -c key=value`
  として、既知の安全値（下記）以外は実行内容に関わらず Deny（`GIT_PAGER=less`・`GIT_EDITOR=vim` は Allow）。
  **ban が扱わない形は残る**（ban 理由の文言は「blastguard が認識する形」だけを主張する）: `PAGER`/`EDITOR`/
  `VISUAL`（git も読むが git 固有でないので従来どおり値を shell line として解析するだけ）、program を指す
  `sendemail.smtpServer`、`instaweb --httpd`、既に config ファイルに保存された設定。
  `filter-branch` は従来どおり `irreversible_git_subcommand` が全用途を Deny。埋め込みコマンドが spelled out
  されていてそれ自体が Deny のとき（`git bisect run rm -rf /usr`）はその Deny 理由を返す（system-harm
  marker を保つため。`detect.rs` の git arm）。非対象（Allow のまま）: `bisect start/good/bad`、
  `rebase <upstream>`、`-c pager.log=false`、空値（`-c credential.helper=`）、`!` で始まらない alias、
  値が環境変数にある `git --config-env <key>=VAR`（従来の Ask。既存 control が固定）、そして
  **既知の安全値**（ユーザー裁定 2026-11-07「Allow known-safe values」。`detect/executors.rs` の
  `is_safe_command_value`）: 先頭語が key ごとの固定リストにあり、以降が素の flag（`-R`・`--wait` 等。
  `=value`・展開・`;`・quote・operand を含まない）だけの値 — pager（`core.pager`/`pager.<cmd>`）は
  `less`/`more`/`cat`/`delta`、editor（`core.editor`/`sequence.editor`）は `vim`/`vi`/`nvim`/`emacs`/`nano`/
  `code`/`cursor`/`subl`/`true`/`:`（後続語は値を取らない `--wait`/`-w`/`-n`/`--new-window`/`-nw`/
  `--no-window-system` だけ。git が編集ファイルを末尾に足すので `vim -S`・`emacs -l` はそれを code として実行する）、
  `credential.helper`（`credential.<url>.helper` 含む）は `osxkeychain`/`store`/`cache`（`--timeout=<数字>` 可）/
  `manager`/`manager-core`、`gpg.program` は `gpg`/`gpg2`、
  `core.sshCommand` は `ssh`（数字 flag〈`-4`/`-6`〉と flag の後の素の値語〈`-i key`・`-J user@bastion`〉は可。
  `o`/`F`/`I`/`E`/`S` を含む flag〈`-o ProxyCommand=…`・`-F <config>`・PKCS#11 library・log file・control
  socket〉は command を運ぶ／code を load する／file を書くので不可）。`-c` と `git config` 書き込みの両方に適用。リスト外の値
  （`vim; rm -rf ~`・`vim $(…)`・`!…`・`ssh -o ProxyCommand=sh` 等）は ban のまま Deny。
- **system-harm（システムを壊すコマンド）は常に Deny、repeat でも降格しない（backlog fb338793、ユーザー裁定
  2026-11-07「いかなる理由であれシステムに害になるコマンドはdenyすべし」「1は絶対にdenyでいい」）** —
  `rm` の operand が `/` またはシステムディレクトリ（`scope::is_inside_system_dir`: `/bin`・`/etc`・`/usr`・
  `/System`・`/Library`・`/opt`・`/private/etc` 等。`/var`・`/dev` は含まない）の中を指すとき、非再帰の
  1 ファイル（`rm /etc/passwd`・`rm -f /bin/ls`）でも Deny（`detect/rm.rs` の `rm_system_harm_deny`。
  字句の絶対パスと、親ディレクトリを symlink 解決したパスの両方で判定。ワイルドカードは literal prefix で
  判定。セッションの safe root 内〈`/opt/myproj` の project 等〉は対象外）。operand は shell の単語として
  読む（`detect/rm/operand_words.rs`、backlog 3c750df2）: 引用（`'…'`・`"…"`・`$'…'`〈`\xHH`・8 進含む〉・
  連結・backslash）を外し、brace のカンマ列は展開、連番 `{a..b}` は `*` 扱い、`~`・`$HOME`・`${HOME}` は
  home へ展開（home 起点で `..` が home より上へ登る綴り〈`~/../../etc/passwd`〉は `/` 起点としても判定）。
  それ以外の展開（`$X`・`$(…)`・backtick・`~user`・未対応 escape・不均衡な引用）を含む operand は解決不能
  として飛ばさず、Ask に倒す（ユーザー裁定 2026-11-07「Ask, deny known paths」。subagent では既存の
  Ask→Deny 硬化で Deny。Allow には決してしない）: 他の rm 規則が Deny を返せば Deny、Allow を返すときは Ask。
  ただし同じコマンド行のテキストが値を決める単純な形は解決して判定し、システムパスなら system-harm Deny の
  まま（`detect/rm.rs` の `known_path_harm`）: 同じ行の**前の**セグメントにある literal 代入
  （`X=/etc/passwd; rm $X`・`X=/etc/passwd && rm "$X"`・`"$X/passwd"`。採否は
  `vars::resolve_expanded_command_word` の規則〈条件付き代入・後続の再束縛・配列・空値は不採用〉に従う）と、
  literal 語だけを出力する `$(echo …)`・`$(printf %s …)`・backtick（`rm $(echo /etc/passwd)`）。値が引用符・
  `\`・`$`・backtick・`{}`・`~` を含むもの、引用外の展開が長い語の一部として単語分割されるものは解決しない
  （Ask のまま）。解決して system 外になった operand も Ask のまま（この解決は Deny を足すだけ）。
  引用符内の glob 文字は展開されないので、`-r`/`-d` なしの `rm "*.tmp"` は wildcard rm ではない
  （`for f in *.tmp; do rm "$f"; done` の loop 値代入版がこの形になり、glob は `for` のもの）。loop header の
  語が引用・`\`・`$`・backtick・brace・括弧を含まない素の glob（`*.tmp`・`build/*.o`）なら、各反復の値は
  その glob が一致した 1 エントリなので、本体の引用外の参照（`rm $f`）にも値を `"…"` で囲んで代入する
  （`detect/compound.rs` の `LoopValue::glob_matched`。ユーザー裁定 2026-11-07: `for f in *.tmp; do rm $f; done`
  は wildcard rm Deny ではなく、書かれたままの `$f` の解決不能 Ask。subagent は Deny）。literal prefix による
  system-harm 判定は残る（`for f in /etc/*; do rm $f; done` は Deny）。引用された header 語
  （`for f in "*.tmp"`）は literal な pattern で、本体の引用外 `$f` が実際に glob 展開するので囲まない。その引用除去後の
  operand は protected path（literal として、および pattern として literal prefix で）に照合する。
  展開後もファイル名にならない operand（制御文字・`{}` placeholder）も、他の rm 規則が Allow を返すときだけ
  Ask にする。cwd 不明の相対 operand は従来どおり
  この判定の対象外。redirect 語（`2>/dev/null` 等）は operand に数えない。この Deny、システムディレクトリへの
  書き込み Deny（`system_path_block`）、`crate::deletion` のシステムディレクトリ refusal は理由に
  `model::SYSTEM_HARM_MARK` を含み、`main::downgrade_on_repeat` は `Decision::is_system_harm` の Deny を
  2 度目以降も Ask に落とさない（他の Deny の repeat 降格〈運用者裁定 2026-09-18〉は不変）。
  shape で Deny する規則（`find -delete`・`truncate`・`shred`・再帰 `chmod`/`chown`/`chgrp`・`dd of=`・
  再帰/ワイルドカード `rm`・`osascript` の app delete）は、対象が `/`・システムディレクトリ・raw disk device
  （`/dev/disk*`・`/dev/rdisk*`・`/dev/sd*` 等）・`$HOME` そのもの・home の irreplaceable asset（`~/.ssh`・
  `~/.gnupg`・home 直下の dotfile/dotdir）のとき、同じ理由のまま marker 付きで返す（`detect/harm_mark.rs` の
  `mark_system_harm`。`detect.rs` の command-word loop が `harm_mark::analyze_command_at` 経由で呼ぶ。
  backlog 3c750df2）。Allow/Ask は触らない。置けない operand（展開・quote・base 不明の相対）は mark しない
  （Deny 自体は残る）。システムパスは safe root の strict な内側なら対象外、home 側に escape は無い。
  `git bisect run` の埋め込みコマンドは各語を re-quote して解析するので（`sh -c 'cd /etc && rm passwd'`）、
  その system-harm Deny が ban の理由に代わって返る。`rule_id` は marker を外してから照合する。
- **関数本体の書き込み・rm は呼び出し時点の変数状態で判定する（backlog bdce0261、ユーザー裁定 2026-11-13「A+B」）** —
  裁定原文（Human decisions, 2026-11-13, user ruling "A+B"）:
  - Method: A+B. Judge a function body at each call site with the variable state at that call (late binding).
    If the value at the call resolves to a system path -> Deny (system-harm), consistent with docs/specs/blastguard.md:271-274
    ("Ask, deny known paths"). If the value cannot be resolved -> Ask (Deny in subagents).
  - A body that is defined but never called: keep today's behaviour (judged at definition time).
  - Out of scope, stated explicitly: `function f {}` spelling, subshell bodies `f() ( ... )`, calls through `eval` / `command`,
    and functions defined in a different Bash tool call than the one that calls them. A definition and a call on different
    physical lines of the SAME command string (the multiline RED case r2) ARE in scope.
  - Add control c4 to the test file: `L=<safe>; f() { L=/etc/hosts; }; f; echo x > $L` -> not Allow
    (the c4 test must be written by a non-implementer).

  回帰テスト: `crates/blastguard/tests/function_body_late_binding_fail_open_20261113.rs`。
- **rm 以外のシステムファイル変更コマンドも system-harm Deny（backlog 3c750df2）** — `detect/system_paths.rs` の
  `system_mutation_verdict` が `analyze_command_at` の per-command arm より先に 1 箇所で判定する。対象は
  `unlink`/`rmdir` の operand、`mv` の source（`-t` 時は全 operand）、`sed -i`/`--in-place` の入力ファイル
  （`-e`/`-f` が無いときの先頭 operand は script として除外。BSD `-i ''` の空 suffix はファイルではない）、
  `perl -i` の入力ファイル（`-e` が無いときの script ファイルは除外）、`chmod`/`chown`/`chgrp` の target、
  `rsync` の `--del*` 時の destination と `--remove-source-files` 時の source。これらが
  `scope::is_inside_system_dir`（rm と同じ判定。字句の絶対パスと親 symlink 解決の両方、wildcard/brace は
  literal prefix、safe root 内は対象外、`/var/folders`・`/dev/null` は対象外）に入れば
  `Decision::deny_system_harm`。operand は quote-aware に再分割し shell と同じく unquote する。先頭の
  `~`/`$HOME`/`${HOME}` は home に展開し、それ以外の `$`/backtick 展開（`$'…'` 含む）を含む対象 operand は
  **Ask**（判定不能は制限側。例: `chmod +x "$f"`・`mv "$a" b`）。ただし rm と同じく、同じ行のテキストが値を
  決める展開（前セグメントの literal 代入・literal 語の `$(echo …)`）は解決して判定し、システムパスなら
  system-harm Deny（`system_paths::known_mutation_harm`、`rm::known_path_harm` から呼ぶ。
  `X=/etc/passwd; unlink $X`・`X=/etc/hosts; mv $X /tmp/h`・`X=/etc/passwd; chmod 000 $X`）。解決して system 外
  なら Ask のまま（`X=build.log; unlink $X`）。`find -exec` の `{}` と rsync の remote
  operand（`host:/path`）は判定しない。base 不明の相対 operand は rm と同じく判定しない（他の rule は残る）。
- **pipeline で供給される `xargs rm`/`xargs unlink`（backlog 3c750df2）** — `analyze_xargs` は xargs 自身の
  token しか見ないため `echo /etc/passwd | xargs rm` は operand 無しの `rm` として Allow だった。
  `detect/find_xargs.rs` の `xargs_pipe_feed`（`analyze_segment` から `line`/`seg_idx` で呼ぶ）が、
  単一 `|` で xargs segment に供給する前段を読む（stdin のみを読む `sort`/`uniq`/`head`/`tail`/`grep` 系 filter は
  遡って透過。exec wrapper〈`sudo`・`env` 等〉の後ろの program を見る）。前段が `echo`/`printf` の operand・
  `find` の search root・`ls` の operand でシステムディレクトリ内を指せば `rm_system_harm_deny` と同じ判定で
  **Deny（system harm）**（相対パスは raw base〈直前の `cd` 先〉に置いて判定。置けなければ Ask）。前段の出力が
  コマンド行から分からない（`cat FILE`・`curl`・展開 `$X`・未知 program・`git -C`）ときは **Ask**（subagent では
  hardened Deny）。非システムの literal パス・`git ls-files`/`diff`/`diff-tree`/`ls-tree`（repo 内）は従来どおり
  （この規則は判定を足さない）。前段が無い・`-a FILE`・remover 以外の xargs は対象外。
- **埋め込み shell 文字列の Ask と osascript の削除（backlog 0fefcd3b）** — interpreter program の
  文字列 literal（layer > 0）に対する shell pass の **Ask** は、program が shell に文字列を渡す API
  （`SHELL_STRING_EXEC_MARKERS`: `os.system`/`os.popen`/`subprocess`/`system(`/backtick/`child_process`/
  `do shell script` 等）を含むとき pending Ask として残す（従来は捨てられ、
  `os.system('echo x > .git/hooks/pre-commit')` が Allow だった。top-level の同じ redirect は保護パス Ask で、
  人間不在時は Deny）。「command word が展開」由来の Ask（`detect::EXPANSION_HEAD_TAIL`）は残さない（`os.system('echo "$HOME"')`・argv list `['echo','$HOME']` の literal は data。`interpreter::is_expansion_head_ask` で除外、blastguard 0.2.301）。
  `osascript` は、program の文字列 literal の外に削除動詞（`delete`・`empty … trash`・`move … trash`）があり、
  かつ（a）文字列 literal で `Finder`/`System Events`/`com.apple.finder`/`com.apple.systemevents` を名指しする
  （`tell application id "…"`・JXA `Application("Finder")` を含む）か、（b）`application`/`app` の対象が単一の
  plain 文字列 literal でない（`"Fin" & "der"`・`+` 連結・変数）とき system-harm Deny
  （`Decision::deny_system_harm`。repeat も Deny）。program は届き方を問わず判定する: `-e`・here-string
  （`osascript_app_delete`）、here-document（quoted/unquoted/`-`、`-l JavaScript` も）と `echo`/`printf` の
  pipe（`detect/payload.rs` の stdin program text → `osascript_program_delete`）。言語が不明なので
  AppleScript（`"` のみ）と JXA（`"`/`'`/`` ` ``）の 2 通りで字句解析し、どちらかで該当すれば Deny
  （backlog 3c750df2）。`display notification "x"`・Finder の get/open/reveal は従来どおり Allow。
- **interpreter program の OS 呼び出し（backlog 3c750df2、`detect/interpreter/shell_calls.rs`）** —
  layer-0 の program を Python 規則で字句解析し（`#` comment・triple quote・`r`/`b`/`f` prefix）、**code 上の
  呼び出し**だけを読む。文字列 literal はその呼び出しの引数であるときだけ shell / argv として扱い、それ以外は
  data（`note = "os.system('$CMD')"`・`s.replace("`$CMD`", "")` は判定しない）。
  (1) path 変更系（`rename`・`remove`・`unlink`・`rmdir`・`rmtree`・`move`・`truncate`・`chmod`・`chown` 等の
  最終 component、`os.replace`）の文字列引数がシステムディレクトリ内なら system-harm Deny
  （`os.rename('/etc/hosts','/tmp/x')`）。(2) `os.exec*`/`os.spawn*`/`os.posix_spawn*` は `(path, argv0, args…)`
  を path＋args の argv に組み直し、argv list の `subprocess.*` と同様に判定（Deny のみ採用）。
  (3) `os.system`/`popen`/`getoutput`/`getstatusoutput`/`subprocess.*(…, shell=True)` の literal 第 1 引数は
  shell 行として full shell analyser で判定し、Ask も残す（`os.system('$CMD')` は Ask、subagent では Deny）。
  (2)(3) の argv / 単純コマンドが `rm`/`rmdir`/`unlink`/`mv`/`shred`/`truncate`/`chmod`/`chown`/`chgrp` で
  いずれかの operand が、または `cp`/`ln`/`install` で最後の operand がシステムディレクトリ内なら
  system-harm Deny（`os.system('mv /etc/hosts /tmp/x')`）。引数が literal でない呼び出し（変数）は
  ここでは判定せず、従来の program audit に残る（未対応の residual）。
  (1') **Python が読むプログラム（`python -c`・stdin から読む program）は、path 変更系呼び出しの path 引数を
  字句の一致ではなく静的評価で解決して判定する**（backlog f6dbff5b、blastguard 0.2.523、
  `interpreter/python_semantics.rs` の `path_mutation_verdict`）。import alias を正規名に書き換えた上で、
  here-doc の Python file 判定と同じ `Eval::expr`（隣接 literal の暗黙連結 `'/' 'etc/hosts'`・`+`・
  `os.path.join`・`/`・単純代入された名前）で path を解決し、下の「filesystem 破壊」と同じ shell 行
  （`shutil.rmtree`/`os.remove` 等 → `rm`、copy/move/rename の転送先・`os.truncate` → `: >`、
  `os.chmod`/`os.lchmod`/`Path.chmod()`/`.lchmod()` → `chmod <mode>`、`os.chown`/`os.lchown`/`shutil.chown`
  → `chown 0:0`）として shell 規則で判定する。`os.chmod('/''etc/hosts',0)` は system-harm Deny、
  `os.chmod('a' 'b',0o644)`・`os.chmod('build/x',0o755)` は Allow。**path を静的に解決できない呼び出しは
  Ask**（subagent では Deny。`os.chmod(sys.argv[1],0o755)`）。0.2.526 以降（backlog 17d985f1）は
  `open`/`os.open`/`.open()`/`write_text`/`write_bytes` もここで判定する（literal トークンだけを読む従来の
  write-target 走査にも引き続きかかる）。pathlib の書き込み method（`write_text`/`write_bytes`/`touch`/
  `symlink_to`/`hardlink_to`/`chmod`/`unlink`/`rmdir`/`.open(書き込み mode)`）は **receiver** を
  `Eval`（`/`・`joinpath`・括弧・単純代入された名前）で解決して判定する: `(Path('/etc') / 'hosts').write_text('x')`・
  `Path('/etc').joinpath('hosts').write_text('x')` は `: > /etc/hosts` として system-harm Deny、
  `(Path('build') / 'x').write_text('y')`・`Path('build').joinpath('x').write_text('y')` は Allow。
  `touch` は `: >> p`、`symlink_to`/`hardlink_to` は `: > p`。引数がちょうど 1 個（位置引数か `target=`）の
  `.replace(…)`/`.rename(…)` は `Path.replace`/`Path.rename` として、**転送先（引数）を `: > dst`、receiver を
  `rm src`** として判定する（`Path('/tmp/y').replace('/etc/hosts')` は Deny、`Path('a').replace('b')` は Allow）。
  0.2.542 以降（backlog c8bf27ea）は同じ callable を直接呼び出し以外で使う形（unbound `Path.m(recv, …)`・
  `w = X.m; w(…)`・`.__call__`・引数や container に渡す参照等）も判定する（下の「メソッド参照」項）。
  引数が 2 個以上の `str.replace('a','b')` や `target` 以外の keyword（`dt.replace(year=…)`）は対象外。
  receiver・転送先・path が解決できないものは Ask（subagent では Deny）: 変数 receiver で束縛が読めない
  `p.write_text(..)`・1 引数の `x.replace(y)`/`x.rename(y)`（pandas の `s.rename('n')` も含む）・
  f-string で組んだ `open(f'{d}/x','w')` などは 0.2.523 の Allow から Ask に変わる。
  0.2.544 以降（backlog 67219006）、解決した書き込み先が shell の redirect 規則で**特定できない**もの
  （payload に `cwd` が無いときの相対 path、`$`・backtick を含む path）は、redirect 規則の Deny ではなく
  **保留 Ask**（subagent では Deny）にする。program-text の shell pass と同じ R2 の side channel
  （`ProgramTextSink`、`redirects_are_data: false`）経由で判定するので、system directory・保護 path・
  既存の復元不能 target など redirect 規則の他の拒否はそのまま Deny、同じ program の他の Deny が優先する。
  `python3 -c "open('a/b','w')"`・`Path('a/b').write_text('x')`（cwd 無し）は 0.2.526〜0.2.543 の Deny から
  Ask に戻る（write-target 走査の `Recovery::Undetermined` = 保留 Ask と同じ結論、backlog 7483e85b）。
  実際の shell の `echo x > a/b`（cwd 無し）は Deny のまま。
  chmod の mode は Python の整数 literal（`0o755`・`493`・`0x1ff`）ならその値、それ以外は `000`
  （読み取り・実行を外す＝shell の chmod 規則で最も制限側）として描画する。
  (4)（backlog 569b9b9d、blastguard 0.2.518）`Path(…)`/`pathlib.Path(…)`/`PosixPath(…)`/`WindowsPath(…)` の
  呼び出し結果に**直接**続く method（(1) の path 変更系、および `replace`・`write_text`・`write_bytes`・`touch`・
  `symlink_to`・`hardlink_to`）は、receiver の文字列引数がシステムディレクトリ内なら system-harm Deny
  （`Path('/etc/hosts').chmod(0o777)`・`pathlib.Path('/etc/hosts').write_text('x')`・`Path('/etc/hosts').unlink()`）。
  (5) copy 系（`shutil.copy`・bare `copy`・最終 component が `copy2`/`copyfile`/`copytree`/`copyFile`/
  `copyFileSync`/`cpSync`）は第 1 引数（読むだけの転送元）より後の文字列引数がシステムディレクトリ内なら
  system-harm Deny（`shutil.copy('/tmp/x','/etc/hosts')`。`shutil.copy('/etc/hosts','/tmp/x')` は対象外）。
  変数に束縛した path（`p = Path('/etc/hosts'); p.chmod(…)`）は (4) では判定しない（residual）。
- **interpreter の書き込み literal にもシステムディレクトリ軸（backlog 0fbfe00d、blastguard 0.2.395）** —
  program audit の回復可能性 probe（`detect/interpreter.rs`）は存在しない path に `NothingToDestroy` を返すので、
  `shutil.copy2(src, "/etc/x")`・`open("/etc/x","w")` のような**新規ファイル**作成が Allow だった（直接形
  `cp`/`>`/`tee` は Deny）。書き込み呼び出しを含む program の write-target literal（probe と同じ条件、probe 上限を
  超えた literal も含む）に `system_path_block` → `real_system_path_block` を適用し、Deny はその場で返す。
  symlink を解決できない landing は Ask（pending、後続の Deny に負ける）。相対 literal は判定しない（従来どおり）。
  書き込み呼び出しの綴り（`INTERPRETER_WRITE_CALLS`）には blastguard 0.2.518 で node の `appendFileSync(`/
  `appendFile(` と pathlib の `.write_text(`/`.write_bytes(` を追加した（backlog 569b9b9d）。
- **`Library/LaunchAgents` への新規ファイルは Ask（ユーザー裁定 2026-11-07、backlog e34eb0be、blastguard 0.2.395）** —
  `system_path_block` が最初に `launch_agents_new_file_ask` を見る: 正規化した絶対パス（先頭 `~/` は `$HOME` で展開）に
  `Library/LaunchAgents` の component 対（ASCII 大小無視）とその下の component があり、システムディレクトリ外で、
  対象が存在しないとき Ask（subagent では Deny）。`confined_root` で逃げられない。存在を読めないときも Ask。
  既存ファイルへの書き込みは対象外で、呼び出し側の回復可能性規則に委ねる（Ask で Deny を降格させないため）。
  直接形（`>`・`>>`・`tee`・`cp`/`install`・file tools）と interpreter 形の両方に効く。
  併せて、quoted-delimiter here-document を Python が読む body 行（`python_program_body_segments`）では、
  top-level segment 解析の「command word が展開」Ask（`EXPANSION_HEAD_TAIL`）を落とす
  （`python_body_segment_verdict`。shell はその body を parse しない。program が実際に shell に渡すかは
  上記 (3) が判定する）。
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
- **inline program は原文のまま判定する（`detect/payload.rs` の `faithful_tail`、
  `detect/interpreter/invocation.rs` の `analyze_code_interpreter`、backlog c619b13f、blastguard 0.2.481）** —
  python / node / perl / ruby / php / deno の `-c`/`-e`（くっついた `-c"…"`、`--eval=` を含む）の inline program、here-string・
  プロセス置換で interpreter に渡る program、`sh -c`／`bash -c`／`flock -c`／`eval`／`exec`／`source`／`.`、
  `find -exec`／`xargs` 経由の同等形、`cd X && …` の書き換え後の行は、空白分割した語を空白で繋ぎ直した文字列
  ではなく、segment 原文から切り出した**改行・連続空白を保った原文**（shell の quote を外したもの）で判定する。
  繋ぎ直しでは改行が潰れ、`#` コメントが次行の system-harm 呼び出しを飲み込んで Allow になっていた
  （critical fail-open）。原文が切り出せず語が複数トークンにまたがる場合（`word_separators_lost`）は Allow を Ask に
  倒す（判定不能は制限側）。blastguard 0.2.522（backlog e4523bfd、spec blastguard-comment-collapse-residual
  R1/R2）以降、未知 wrapper の後ろの行も原文で判定し（`detect/payload.rs` の `reanalysis_text`。原文が読めず
  区切りが失われたら Ask）。blastguard 0.2.525 以降、`env -S`／`--split-string` の operand は env 自身の分割規則
  （GNU coreutils `build_argv` と FreeBSD/macOS `split_spaces` の共通部分）で argv に分割し `env <argv…>` として判定する
  （`detect/executors.rs` の `env_split_argv` / `env_split_string_verdict`）。`$`／`${VAR}`、両実装で結果が違うか拒否される
  エスケープ、閉じない quote、shell 側の展開など規則で確定できないものは Ask（subagent では Deny）。従来の shell 文字列
  としての読みも追加の読みとして判定する。`env -S 'ls   -la'` は Allow のまま。
- **`open()` 系呼び出しの mode は構文で判定する（`detect/interpreter/open_mode.rs`、backlog 6cac93af、
  blastguard 0.2.493）** — inline program 中の `open` / `fdopen` / `opensync`（`io.` / `codecs.` / `os.` 等の
  receiver 付きを含む）の呼び出しについて、mode operand を位置引数・`mode=` から取り出し、literal が
  `w` / `a` / `x` / `+` / `>` のいずれかを含めば書き込みとして書き込み先 probe に掛ける（`'a'`・`'ab'`・`'r+'`・
  `'x'`・`mode='a'` を含む）。mode が静的に決まらない（変数・式・f-string・splat・閉じない引数列）なら書き込み扱い、
  整数は `os.open` flags として `0` のみ読み取り、mode 省略は読み取り。呼び出しでない `open` の出現
  （alias・`partial(open, …)` 等）は program 全体を判定不能＝書き込み扱いにする。既知の python（layer 0、
  `exec`/`eval`/`getattr` 系の動的実行印も f-string も無い）は Python lexer でコードトークンだけを数え、
  文字列・`#` コメント中の `open` は数えない。コードトークンに非 ASCII 文字があれば判定不能（NFKC 正規化で
  `ｏｐｅｎ` が `open` になるため）。それ以外は原文走査で、どこかに非 ASCII 英数字があれば判定不能。
  shell の quote 除去（`detect/words.rs` の `first_shell_word`）は POSIX の行継続を実装し、unquoted と
  double quote 内の `\`＋改行を取り除く（single quote 内は保持）。トップレベルのコマンド語・引数の行継続は
  下の「行継続を除いた読みも判定する」で扱う（backlog 040b959b）。Python 文字列 literal 内の行継続は
  未対応（RED 06143f1b、別 backlog）。
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
- **関数本体は呼び出し時点の変数で判定する（`detect/function_calls.rs` の `function_call_reading`、
  backlog bdce0261、blastguard 0.2.555）** — `f() { …; }` / `function f { …; }` の定義を `:` に、各呼び出しを
  その本体の `{ … }` group に置き換えた読みを、`detect_bash_every_reading`（トップレベル）と
  `shell_payload_verdict`（`bash -c` 等の payload）で原文の読みに**加えて**判定し、厳しい方を返す
  （`L=<safe>; f() { echo x > $L; }; L=/etc/hosts; f` は system-harm Deny）。inline できない呼び出し
  （`f() ( … )` 等の未モデル化の定義形、定義への redirect、閉じない本体、深さ 8 を超える再帰、
  256KiB を超える展開、segment 対応付けの失敗）は判定不能として Ask（subagent では Deny）。
  `$(f)`・backtick・`eval`/`command`/`xargs`/`env` 経由の呼び出しはこの読みでは認識せず、原文の読みだけが
  判定する（未検証の残余）。回帰テスト: `crates/blastguard/tests/function_body_late_binding_fail_open_20261113.rs`。
- **pipe で `( … )` / `{ …; }` / `. /dev/stdin` に渡る program も判定する（`detect/payload.rs` の
  `group_reader_pipelines`、backlog 036067bf、blastguard 0.2.555）** — pipe で供給される stage が subshell・
  brace group なら、group 内の各 command を `UPSTREAM | member` として `analyze_interpreter_stdin_exec` で
  再判定する（入れ子は 1 層ずつ剥がす）。閉じ括弧が見つからなければ Ask。`.`/`source` は pipe または
  here-document で供給され第 1 引数が `/dev/stdin`・`/dev/fd/0`・`/proc/self/fd/0` のとき stdin reader と
  みなし、未モデル化の option は Ask。回帰テスト:
  `crates/blastguard/tests/backlog_20261114_pipe_into_subshell_brace_source_reader.rs`。
- **pipe 先の shell の argv は option 文法で読む（`detect/payload.rs` の pipe 規則、
  `detect/payload/program_source.rs` の `shell_program_source`、backlog de914866、blastguard 0.2.556）** —
  shell reader（`is_shell`）では `-o`/`-O`/`--rcfile` の値や `-s` 以降の位置引数を script file と誤認しない:
  `File` なら script file 実行、`Stdin` なら pipe の program を判定、`Inline`（`-c`）は従来の読み、
  `Undetermined`（未知の option）は pipe/here-document 供給時に Ask を記録したうえで program も判定する。
  code interpreter は従来の `stage_has_program_operand` のまま。回帰テスト:
  `crates/blastguard/tests/backlog_20261114_pipe_to_shell_with_value_flags_or_s_args.rs`。
- **compound command 内・exec fd・zsh `=()` 経由の shell program も判定する（`detect/payload.rs` の
  `decompounded`、`detect/payload/exec_fds.rs` の `exec_opened_fds`、backlog 72872200、blastguard 0.2.557）** —
  `analyze_interpreter_stdin_exec` は行を原文と compound 構文を外した読みの 2 回判定する（追加のみ。2 つの
  splitter の長さが食い違えば Ask）。`exec N< <(…)` / `exec < <(…)` で開かれた fd から stdin を複製する reader は
  その procsub の program を判定し、どこでも開かれていない fd（`bash <&3`）や here-string/here-document で開かれた
  fd は、reader が stdin から program を読みうるとき Ask。ファイルで開かれた fd は `bash < file` と同じ扱い。
  exec で開かれた stdin は upstream pipe のある reader にも適用する（制限側の過大近似）。zsh の `=(…)` は
  process substitution として読む。回帰テスト:
  `crates/blastguard/tests/backlog_20261114_compound_wrapped_or_cross_stage_shell_program.rs`。
- **inline Python program の文字列中の `>` は data（#1626、ユーザー裁定 2026-11-07）** —
  `python3 -c '…'`（`python`/`python3`/`python3.N` の inline-eval のみ）の program text に対する
  shell pass で、truncating redirect の target が**判定可能**なものは recoverability による Deny を
  出さない（`detect/interpreter.rs` の `ProgramTextSink::redirects_are_data`）。条件は全 payload が:
  Python tokenizer で tokenise できる（`python_file.rs` の `code_view`）・`CODE_BUILDING` marker
  （`exec(`/`eval(`/`getattr(`/`__import__` 等）が raw にも code view にも無い・相対 import／runner
  module からの star import が無く import は全て `DATA_ONLY_MODULES`（sys/os/json/re/glob/pathlib 等の
  allowlist）・alias 解決後の code が process を起こす名前を参照しない（`python_semantics.rs` の
  `references_runner`: `RUNNERS`、裸の runner module、`subprocess`/`pty`/`commands`/`asyncio` の属性、
  最終成分が `system`/`popen`/`exec*`/`spawn*`/`posix_spawn*`/`fork` 等の名前。`ctypes.CDLL(None).execl`
  のような呼び出し結果の属性も含む）・`SHELL_STRING_EXEC_MARKERS` と動的実行 marker が文字列を含め
  どこにも無い。`python3 -c 'import sys; s="> F"; print(s)'` は Allow。判定不能な target は従来どおり
  R2 の Ask、redirect arm の先行する拒否（保護パス・system directory・置けない target）はそのまま。
  条件外の program は従来どおり判定する。program 語（`-c` の直後の語）が Python として tokenise
  できない（閉じない文字列・triple quote・括弧の不均衡）ときは、他に Deny が無ければ Ask（Allow しない）。
- **大文字小文字だけ違う綴りは symlink ではない（#1630）** — `SafeRoots::no_symlink_below` は、
  root 以下の component の realpath が `<親の real>/<綴った名前>` と**大文字小文字だけ**違うとき、
  同じ親・両名の `lstat` がどちらも symlink でない・`(st_dev, st_ino)` が一致する、を全て観測した場合に
  限り同じ entry とみなす（`scope/case_fold.rs` の `is_case_only_same_entry`。case-insensitive APFS で
  on-disk `mergeMain.log` に `> mergemain.log` と書いた scratchpad redirect が Allow になる）。
  観測できなければ従来どおり「root 以下の symlink」（Allow しない）。本物の symlink は `lstat` で落ちる。
- **chain された行の worktree checkout への redirect（#1625）** — git で復元できる tracked file への
  truncating redirect の whose-tree 判定（`Ctx::in_worktree_checkout_for_redirect`、保護パス免除には
  使わない）は、単一 plain command に加え、redirect を含む segment までが行頭から `&&` だけで繋がり、
  それより前の segment が `cd <literal 絶対 dir>`（先頭のみ）・literal 代入（`PATH`/`IFS`/`CDPATH`/
  `BASH*`/`LD_*`/`DYLD_*` 等の shell 挙動変数と、hook の環境に既にある名前は不可）・
  `cp`（option 無し、`$NAME` は同じ prefix で代入済みの値に置換して `-` 始まりや glob が無いこと）/
  `echo`/`printf`/`true`/`pwd`/`ls`/`cat`/`test`（quote・`>` 無し）だけで、どの segment にも
  `` ` ``・`(`・`)`・`{`・`}`・`<`・`\`・`&`・改行が無く、write segment の後が `|`/`&` でないときも、
  target が worktree storage root 下の linked worktree checkout 内（symlink 無し）なら session の tree と
  みなす（`detect/rm.rs` の `write_prefix_cannot_relink`）。それ以外は従来どおり拒否。
- **同じ行で relink された redirect target は実際の着地点で判定する（tests/scope_redirect_relaxation_boundaries_20261108.rs
  の `wt_relink_prefix_from_linked_worktree_cwd_pre_existing`）** — `>`/`>>` の target（`place_redirect_target`
  で配置済み）について、保護パス判定の直後に `detect/relink.rs` の `redirect_landing` が行の他 segment を見る。
  対象は write より前の segment、`|`/`&` で並行する後続 segment、loop（`done` を含む行）なら全 segment。
  model する command（先頭の代入と `sudo`/`env`/`command`/`nohup`/`time`/`nice`/`exec`/`builtin` は読み飛ばす）:
  `ln -s`/`cp -s`（symlink、相対 source は link の親から解決）・`ln`/`link`（hard link）・`mv`・
  link を保つ `cp`（`-R`/`-r`/`-a`/`-P`/`-d` 等。copy 部分に symlink が無ければ copy 先＝target のまま）。
  option 無しの `cp` と `install`（`-l` 以外）は symlink を作らないので inert。これらが target かその祖先を
  作る／置き換えるとき、target を「source ＋ 残りの path」の物理着地点（既存 prefix の symlink を全て解決）に
  置き換え、以後の全 axis（保護パス・system directory・recoverability・whose-tree）をその path で判定する。
  直接その path に書いたのと同じ判定になる（`cd <wt> && ln -sf <他 project の tracked file> F && git show HEAD:F > F`
  は whose-tree の Deny、`ln -sf /etc/hosts F && echo x > F` は system directory の Deny）。tree 内を指す
  link は tree 内で判定されるので従来の Allow のまま。判定不能 — 非 literal な source/destination・Unknown cwd・
  `rsync`/`install -l`・source 自体が同じ行で relink される連鎖・2 つ以上の command が同じ target を relink・
  model しない command（`sh -c`・`python3 -c`・`git -C D mv` 等）が relink 語（`ln`/`mv`/`cp`/`rsync`/`install`/
  `ditto`/`symlink`/`rename` 等）と、target の path 成分または非 literal な語を両方含む — は Ask（Allow しない）。
  `/dev/null`・`/dev/stdout`・`/dev/stderr` は対象外。**RESIDUAL**: relink 語も target も文面に現れない
  command による relink（archive 展開・build script・tracked symlink の `git checkout`）は model しない。
- **同じ行で relink された command operand も実際の着地点で判定する（tests/relink_shapes_20261111.rs の
  r4/r5/r8）** — `detect_bash` の per-segment loop で、segment をそのまま判定する前に
  `relink::operand_landing_verdict` が上と同じ relink model（同じ segment 範囲・同じ判定不能条件）を path で
  判定される operand に適用する: `cp`/`mv`/`install`/`ln` の destination（`-t`/`--target-directory=` 含む）、
  `mv` の source、`rm`/`unlink`/`rmdir`・`tee`/`touch`/`truncate`/`shred`/`chmod`/`chown`/`chgrp` の operand、
  `sed -i` の file、`sort -o` の出力、`uniq` の出力 operand。最後の成分を辿らない operand（`rm`/`unlink`/`rmdir`・
  `mv` の source・`-T` か `ln -n`/`-h` の destination。末尾 `/` は除く）は親 directory だけを着地点へ解決し
  名前を付け足す（`ln -s /etc d && rm d` は link を消すだけなので不変）。それ以外は path 全体を解決する。
  着地点が hook 時点の物理 path と異なる write operand（削除・`mv` の source 以外）は、まず着地点そのものを
  redirect と同じ保護パス axis と system directory axis（`'<verb> target <着地点>' writes into a system
  directory`）で判定する — verb 自身の rule に system path axis が無い `sort -o`・`touch`・`truncate` も
  着地点を名指しで Deny する（tests/relink_operands_20261112.rs の h2/h4/h6）。次に、その operand をその物理着地点へ書き換えた segment を、全 rule で**追加で**
  判定する（書いたままの判定は残るので verdict は厳しくなる方向にしか動かない）。`ln -sfn /etc src/d &&
  rm -rf src/d/hosts`・`… && cp src/a.rs src/d/hosts`・`… && tee d/hosts` は着地点（macOS では
  `/private/etc/hosts`）の system directory Deny。判定不能は Ask。relink 語を含まない行では何もしない。
  `git mv SRC… DEST`（`git` と `mv` の間に option が無い形）は `mv` として model する（redirect 側も同じ）。
  model しない relink 語扱いにすると、同じ path 成分を含む無関係な operand まで Ask になる
  （`git mv src/a.rs src/b.rs && rm src/c.rs` は Allow のまま。k4）。`git -C D mv` 等は従来どおり model しない。
- **非 shell・非 Python のコードファイルへ書く quoted here-document の本文は shell 行として読まない
  （user ruling 2026-11-08、issue #1602、tests/backlog_cd_redirect_fp_20261107.rs）** — `detect_bash` の
  手順 0c。`heredoc::non_shell_code_file_here_document` が、`.py` の手順 0b と同じ形（前行は
  `permitted_prefix_line`、opener 行は 1 個の here-document・`$`/backtick 無し・quote 均衡、区切りは
  **引用付き**、本文は終端行で閉じ空でない、奇妙な空白無し）で、読み手が `cat`/`tee`、書き込み先が**全て**
  literal path で拡張子が `NON_SHELL_CODE_EXTENSIONS`（`rs`・`go`・`c`・`h`・`cc`・`cpp`・`hpp`・`java`・
  `kt`・`swift`・`ts`・`tsx`・`js`・`jsx`）のとき本文を除く。さらに本文 1 行目が `#!` 行でない（`#![`
  は Rust の inner attribute なので可）、終端後の各行は空行・行頭 comment・または `INERT_LATER_COMMANDS`
  （`ls`・`cat`・`head`・`tail`・`wc`・`grep`・`cmp`・`stat`・`pwd`・`echo`・`printf`・`true`。
  program を起動する option を持つ tool は載せない: `rg` は `--pre`/`--hostname-bin` で任意 program を
  実行するので除外〈tests/non_shell_code_heredoc_failopen_probe_20261111.rs〉、`diff`・`file` も除外）を先頭語とする simple command の並びで、`$`・backtick・`\`・`(`・`)`・`{`・`}`・`<`・
  `>`・`#`・不均衡 quote を含まないこと。書いたファイルを実行・source・build しうる後続
  （`cargo test`・`rustc x.rs`・`sh x.rs`・未知の語）が 1 つでもあれば本文は従来どおり shell 行として判定する。
  `.sh`・拡張子無し・他の拡張子は従来どおり shell 判定。opener 行と後続行は本文を除いた text で全規則が判定する。
- **quoted here-document body の `>` は data（tests/heredoc_body_gt_fp_20261107.rs）** —
  (a) program の文字列 literal（layer > 0）の shell pass が R2 side channel（`Ctx::program_text_redirects`）に
  積む「判定不能な redirect」行は、program が `SHELL_STRING_EXEC_MARKERS` を 1 つも含まないとき捨てる
  （上の 0fefcd3b の Ask と同じ条件。`python3 - <<'EOF'` の `print(a>b, "x->y", '>')` は Allow）。
  layer 0（program text そのもの）の行、literal 由来の Deny、shell に文字列を渡す program
  （`os.system('echo x > /etc/hosts')` は Deny のまま）の行は従来どおり。
  (b) line-level `>`/`>>` scan の redirect view（`redirect_view_consumer`）は、`cd <word>` に加えて
  `permitted_prefix_line` が受理する head（`git add <paths>` 等）、literal 値の単一代入 `NAME=<literal>`、
  body が英数字・空白・`-_./=:,@%+` だけの二重引用 substitution `cd "$(…)"` を剥がす（代入は `;`/`&&`、
  他の 2 つは `&&` の前だけ。`git add a.txt ; git commit -F - <<'EOF'` は tests/heredoc_chain_heads.rs が
  restrictive に固定）
  （`WT=/x; cd "$WT" && git add -A && git commit -F - <<'EOF'`、
  `cd "$(git rev-parse --show-toplevel)" && git commit -F - <<'EOF'` の message 行 `> quoted` を
  未解決 cd 後の相対 redirect と誤認しない）。unquoted の `$(…)`・それ以外の head は剥がさない。
  opener 行は view に残るので、opener の redirect（`cat <<'EOF' > /etc/hosts`）は従来どおり Deny。
- **`git commit -F - <<'EOF'` の後ろに続く opener 行のコマンド（tests/commit_msg_heredoc_trailing_cmd_fp_20261107.rs、
  backlog ffce9c59）** — `strip_here_document_body`（全ルール前の data strip と redirect view の両方）は、
  区切りが引用付きで opener の consumer が `git … commit … -F - <<'EOF' [2>&1] <op> <tail>`
  （`<op>` は `|`・`;`・`&&`・`||`）のとき、`<op>` より前（`2>&1` を除く）を読み手として判定する
  （`git_commit_reader_before_tail`）。here-document は付いた simple command の stdin だけを差し替えるので、
  `|` 以降は git の stdout を、`;`/`&&`/`||` 以降は本文を見ない。tail は command text に残り独立に判定される
  （`<<'EOF'; rm -rf /etc` は Deny）。consumer に `\`・`$`・backtick、単独 `&`・`|&`、読み手の出力 redirect、
  git に繋がる pipe 段の program が `cat`/`grep`/`egrep`/`fgrep`/`head`/`tail`/`wc` 以外か `<`/`>`/`(`/`)`
  を含む場合は従来どおり本文も判定する（git は subject を stdout に出すため `| sh` は本文を残す）。
- **`.py` へ書く quoted here-document の本文は Python として判定する（user ruling 2026-11-08、
  tests/quoted_heredoc_backtick_fp_20261108.rs）** — `detect_bash` の手順 0b。
  `heredoc::python_file_here_document` が、data strip と同じ形（前行は `permitted_prefix_line`、opener 行は
  1 個の here-document・`$`/backtick 無し・quote 均衡、本文は終端行で閉じる、奇妙な空白無し。opener 行の
  先頭の `cd <literal> &&` に加え `cd <literal>;` も剥がす＝`python_reader_consumer`、
  tests/subagent_heredoc_fp_20261108.rs）で、
  区切りが**引用付き**、読み手が `cat`/`tee` で書き込み先（redirect 先と `tee` operand）が全て literal path で
  うち 1 個以上が `.py`、本文が空でないときだけ本文を切り出す（書き込み先は `targets` に返す）。終端後の行は
  何行あってもよく、本文を除いた command text（`without_body`）に残って shell の全規則で判定される。
  本文は `interpreter::python_file_body_verdict` が判定する（fail-closed。静的に読めないものは Ask）:
  - comment と plain string literal は不活性（docstring の `` `git stash clear` `` は Allow）。
  - **alias 解決**（`interpreter/python_semantics.rs`）: `import X as Y`・`from X import f [as g]`・
    `s = os.system` を正規名に書き換えてから判定する。runner（`os.system`・`subprocess.*`・`os.exec*`・
    `shutil.rmtree` 等）や `os`/`subprocess` module を呼び出し以外で参照（引数として渡す等）したら Ask。
  - OS に command を渡す call は literal 引数から `program_call_verdict_keeping_argv_asks` で shell 規則に
    かける（argv の Ask も保持: `subprocess.check_output(["git","clean","-fdx"])` は git 規則の Ask）。
    `pty.spawn`・`asyncio.create_subprocess_exec` は `subprocess.run`、`asyncio.create_subprocess_shell` は
    `os.system` として判定。f-string の `{…}` 内の code も対象。command が計算される call
    （`computed_shell_call`）と `CODE_BUILDING` marker は Ask。
  - **filesystem 破壊**: `shutil.rmtree(p)` → `rm -rf p`、`os.remove`/`os.unlink`/`os.rmdir`/`os.removedirs`/
    `Path.unlink()`/`.rmdir()` → `rm p`、`open(p,'w'|'x'|…)`・`Path.open('w')`・`write_text`/`write_bytes`
    → `: > p`（`a`/`+` は `: >> p`）、`shutil.copy`/`copy2`/`copyfile`/`copytree`/`move`・
    `os.rename`/`replace`/`renames` の転送先（第 2 引数 / `dst`）→ `: > dst`（move が消す転送元は判定しない。
    tests/heredoc_system_harm_holes_20261111.rs B）、1 引数の `Path.replace(dst)`/`Path.rename(dst)` →
    `: > dst` と receiver の `rm src`、`Path.touch()` → `: >> p`、`symlink_to`/`hardlink_to` → `: > p`
    （backlog 17d985f1）、`os.truncate(p,…)` → `: > p`、`os.chmod`/`os.lchmod`/
    `Path.chmod()`/`.lchmod()` → `chmod <mode> p`（整数 literal でない mode は `000`）、`os.chown`/`os.lchown`/
    `shutil.chown` → `chown 0:0 p`（backlog f6dbff5b）、`os.open` の書き込み flag も同様として shell 規則で判定（home・`/`・
    dotfile は shell と同じ Deny/Ask、repo 相対は Allow）。path は literal・`expanduser`・`Path.home()`・
    `os.path.join`・`/`・`joinpath` と、単純代入 `name = …` だけで束縛される名前（全代入の値を全部判定）から
    静的に解決し、解決できない path・literal でない mode/flag は Ask。
  - **メソッド参照（backlog c8bf27ea、blastguard 0.2.542、`interpreter/python_semantics/method_refs.rs`）**:
    上の path 判定（here-doc の Python file と `python -c`/stdin の program の両方）は、書き込み・移動系の
    callable（pathlib の `write_text`/`write_bytes`/`touch`/`symlink_to`/`hardlink_to`/`chmod`/`lchmod`/`unlink`/
    `rmdir`/`replace`/`rename`/`open` と、`os.remove` 等・`shutil.copy*`/`move`・`open` 等の module 関数）を
    直接呼び出し以外で使う形も読む。`X.m.__call__(…)` は `X.m(…)`、unbound の `Path.m(recv, …)`（`PosixPath`/
    `WindowsPath` も）は第 1 引数（か `self=`）を receiver・残りを bound 形の引数として `recv.m(…)`、
    全束縛が単純代入 `w = <参照>` の名前の `w(…)` は参照先の呼び出しとして（書かれたままの呼び出しとしても）
    判定する。束縛そのものを判定しないのは、全束縛が **module 直下の文**（行頭 column 0 で、同じ行の前に `:` が
    無い）で、`w` が直接呼び出し（`w(…)`/`w.__call__(…)`）以外に現れず属性（`C.w`・`self.w`）としても
    現れないときだけ。class 属性（`class C:` 本体の `w = …` → `C.w('x')`。round 2 の RED）・`self.w =`/`o.w =`・
    subscript・連鎖/tuple/注釈付き/augmented の代入先・関数 local に入れた参照は下の「それ以外の参照」として
    判定する（`Path.replace(Path('/tmp/z'),'/etc/hosts')`・
    `w = Path('/etc/hosts').write_text; w('x')`・`Path('/etc/hosts').write_text.__call__('x')` は Deny、
    `w = Path('build/x').write_text; w('y')`・`Path.replace(Path('a'),'b')` は Allow）。**それ以外の参照**
    （別の名前へ渡す・引数・container・`.__get__` 等の属性・walrus・literal 名の `getattr`/
    `operator.methodcaller`/`operator.attrgetter`）は構造的に fail-closed: receiver が解決できれば直接呼び出しと
    同じ shell 行（`chmod` は mode `000`、`open` は `: >`）で判定し、解決できない receiver・unbound の class 参照・
    module 関数の参照・`replace`/`rename` の転送先は Ask（subagent では Deny）。Allow のまま残すのは
    receiver が文字列 literal（`'a'.replace`）か再束縛されていない `str`/`bytes`/`bytearray`（`str.replace`）、
    `pathlib`/`os`/`posix`/`shutil`/`io`/`codecs`/`builtins` 以外の import した module の属性（`webbrowser.open`）、
    receiver が既知の非 path な stdlib constructor（`zipfile.ZipFile`/`PyZipFile`・`tarfile.open`/`TarFile`。
    `import zipfile` の綴りのみ）の呼び出しそのもの、またはその呼び出しだけで束縛された名前
    （`z = zipfile.ZipFile('a.zip'); o = z.open`）。builtin `map` の第 1 引数に渡した module 関数で、残りの引数が
    文字列 literal だけの list/tuple 1 個のものは literal ごとの呼び出しとして読む: 再束縛されていない
    `open` は 1 引数＝読み込みなので Allow（`map(open, ['a.txt'])`）、`os.remove`/`unlink`/`rmdir`/`removedirs`/
    `shutil.rmtree` は各 literal の `rm` として判定、それ以外の関数・形（`map(open, ['a'], ['w'])`・
    `open = print` 後の `map(open, …)`）は Ask。
    data 属性として同名を読む `args.replace` 等（receiver が解決できない）は Ask になる。未対応（Allow のまま）:
    literal でない属性名の `getattr`、名前以外から辿る callable（`vars(Path)['write_text']` 等）。
  - **計算される書き込み path（user ruling 2026-11-08、`interpreter/python_semantics/values.rs`、
    tests/heredoc_script_failopen_red_20261108.rs の FP 群）**: repo-scoped であると**証明できる**ときだけ Allow。
    それ以外（絶対 root・`~`・`expanduser`・`Path.home()`・`/` や `~` を root とする iteration・出自不明）は Ask。
    - **repo 相対 iteration**: 単純な `for a[, b, c] in <iterable>:` の target は、iterable が
      `glob.glob`/`glob.iglob`（literal pattern、`recursive`/`include_hidden` 以外の keyword 無し）・
      `Path(...).glob`/`rglob`/`iterdir`・`os.walk`（`followlinks` 無し）・`os.listdir`（と `sorted`/`list`/`tuple`
      で包んだもの）のとき値を持つ。値は「repo 相対 root（`./` 除去後に相対・`~` 始まりでない・`..` も
      dot 名 component も無い）以下の何か」で、`os.path.join`/`/` で root 以下に literal（`..` 無し）や
      listing の entry 名を足しても保たれる。`os`/`glob` は素の `import os` だけで束縛され、`sorted` 等は
      再束縛されていないこと（そうでなければ値無し＝Ask）。判定: root が `.`（作業ツリー最上位）で dot 名
      entry を含みうる iteration（`os.walk`・`listdir`・pathlib の glob 系。`glob.glob` は dot 名を拾わない）は
      `.git`/`.githooks`/`.claude` を指しうるので Ask。root が protected path に触れるなら Ask。それ以外は
      等価な shell command — 書き込みは `: > <root>/<entry>`、削除は `rm [-rf] <root>/*` — の shell 判定。
    - **`sys.argv[N]`**: file を実行する後続行が全部 python の直接実行（`python_script_word`）で、各 run が
      N 番目の引数として repo 相対 literal 語（`..`・`~`・`$`・quote・glob 無し、`/`/`-` 始まりでない）を
      渡すときだけ、その literal として判定。引数を渡さない run（`IndexError` になる）も Ask（ruling:
      実際に渡される target が repo 相対 literal のときだけ Allow）。`sys.argv` は `sys.argv[N]` の読み出し
      以外に現れない（代入・`del`・alias・`sys.modules`・`argv` component の別名・`setattr` 等が無い）こと。
    - **`self.attr`**: module 直下の `class C:`（基底は無しか `object`）の method（第 1 引数 `self`）で読む
      `self.x` は、program 中の `.x` への書き込みが全部 `C` 系 class の `__init__` 直下の `self.x = <rhs>` で、
      `<rhs>` が `__init__` の引数（全 `C(...)` 呼び出しの対応引数か default の値）か静的に読める式のとき、
      その値の和集合。`C` は class 文と `C(...)` 呼び出し以外で参照されない。reflection（`setattr`・`getattr`・
      `vars`・`__dict__`・`__class__`・`type`・`super`・`pickle`・`copy` 等）・decorator・`x` を語として含む
      文字列・`x`/`self` の他の束縛があれば値無し（Ask）。
    **RESIDUAL**: iteration root 以下の symlink の先と、root 以下に入れ子になった protected path
    （`crates/x/.claude/settings.json` 等）は見ない（root 自身だけ検査。literal path の shell 規則と同じ）。
    `fp_os_system_git_add`（`os.system('git add -A')`）は shell の `git add -A` と同じ git 規則で Ask/Deny
    のまま（main の shell 判定と一致させており特例化しない）。
  - **import**: 標準ライブラリ外の module・相対 import・runner module からの `import *` は Ask
    （module の code が text に無い。sibling file でありうる）。
  - **SQL**: DB-API driver（`sqlite3` 等）を import する program に限り、`.execute*` の SQL が
    DROP/DELETE/TRUNCATE/ALTER/UPDATE/REPLACE を含む literal、または literal でないなら Ask。
  - Python として token 化できない本文は Ask。
  最終判定は Deny > Ask > Allow で `without_body` の判定と合成する。ただし **書き込み先に `.py` 以外が
  ある**か、**終端後の行が書き込み先を python interpreter（`python`/`python3`/`python3.N`、`env` 経由可）の
  直接の script 引数、または `python3 -m pytest` の唯一の file 引数（option は `-q`/`-v`/`-x`/`-s` の組と
  `--no-header`、fd 複製 `2>&1` のみ。`pytest_file_word`）以外で参照する**（`find -exec`・`xargs`・`sh x.py`・`./x.py`・未知の command。
  参照 = file 名を含む語か glob 文字を含む語。allowlist であり列挙ではない）ときは、本文込みの command 全体も
  shell 規則で判定して合成する（Python の判定だけで決めない）。1 行目が python を含まない `#!` 行のときは
  `None` で従来どおり本文を shell 行としてのみ判定する。`.sh`・拡張子無し・他の言語の拡張子のみの書き込みは
  従来どおり shell 判定。
  **未対応（RESIDUAL）**: 静的解析の外にある効果（`sys.modules`・`globals()` 経由の参照、
  `loop.subprocess_shell` 等の method 経由の spawner、`os.rename`/`shutil.move` の宛先が home 等の場合）は
  拾えない。Python として
  不正な本文（裸の `git stash clear` 行など）は python では実行されないので Allow。後続の別 tool call で
  `sh x.py` されたら、その call の text だけで判定される（data file と同じ residual）。
  **実行時の cwd**: 本文から導いた command（`rm p`・`: > p`・`os.system` の行）は、opener より前の `cd`
  （`cds_before`）が絶対 path 1 個（`.`/`..` component・`~`・glob 無し）なら `cd <dir> && ` を前置して
  （`Ctx::cwd_prefix`）shell 規則の `cd` 追跡で置き場所を判定する。それ以外の `cd`、または終端後の行に
  `cd`/`pushd`/`popd`/`-C`/`--chdir` がある場合は解決不能な `cd "$BLASTGUARD_UNPLACED_CWD" && ` を前置し、
  相対 path は `cd "$X"` 後と同じく制限側（以前は hook の cwd で判定され、`cd /etc && … os.remove('hosts')`
  が Allow だった）。
- **`python3 -` が実行する quoted here-document の本文は、行単位 redirect scan では Python として
  token 化して読む（tests/subagent_heredoc_fp_20261108.rs）** — `redirect_view_without_quoted_here_document_body`
  の data 形が当たらないとき `heredoc::python_stdin_redirect_view`。形は上と同じ（読み手は素の
  `python`/`python3`/`python3.N`、flag のみの option、`-` 以外の operand・redirect 無し。終端後の行は任意で
  view に残る）。`interpreter::python_redirect_view` が本文を token 化し、string literal と `#` comment の
  中身と literal の引用符を空白に置き換える（改行は保持）。ただし中身に `>` と `$`/backtick の両方を含む
  literal・comment は残し（`'`・`"`・`\` だけ空白化）、user ruling R2（2026-10-14: program text 中の判定不能
  な `>` は Ask、Allow にしない）を従来どおり適用する。token 化できない本文は view に本文を残す。
  この view を読むのは行単位の `>`/`>>` scan だけで、interpreter scan 等の他の規則は本文全体を読む
  （stdin の program を Python 判定器だけで判定することはしない）。
- **`python3 -` が実行する quoted here-document の本文は、stdin-program scan に加えて Python 判定器でも読む
  （tests/heredoc_system_harm_holes_20261111.rs A / G）** — 形は上と同じ（`heredoc::python_stdin_here_document`）。
  `payload::analyze_interpreter_stdin_exec` が `interpreter::python_stdin_body_reading` を呼ぶ。token 化
  できない本文はこの読みを持たず、従来どおり interpreter scan だけで判定する。
  - **system harm は Deny**: 判定器（`.py` 本文と同じ `python_program_verdict`）の verdict が system-harm Deny
    （`Decision::is_system_harm`）のときだけ採用する。相対 path は reader の前の `cd` に置く（`.py` 本文と同じ
    `cds_before` 規則。終端後の行は program より後に走るので見ない）ので、`cd /etc && python3 - <<'EOF'` の
    `open('hosts','w')` は `cd /etc && : > hosts` として Deny。それ以外の verdict（Ask、別 work tree の tracked
    file への書き込みのような scope の Deny）は採用せず interpreter scan の判定のまま
    （tests/subagent_heredoc_fp_20261108.rs `control_cd_python_stdin_no_arrow` が Allow を固定）。
  - **string literal は data**: 本文全体が token 化でき、`SHELL_STRING_EXEC_MARKERS`・dynamic-execution marker
    を（string 内も含め）持たず、`python_strings_never_reach_a_shell` を満たし、stdin-program scan の program
    text がちょうどその本文のとき、interpreter scan（`interpreter_code_verdict` の `strings_are_data`）は
    layer 0（program text そのもの）だけを読み、string literal の中身（layer 1 以降）を code として降りない。
    `print` するだけの `"shutil.copy2('a','/etc/hosts')"` は Allow。layer 0 の判定（literal を path として
    見る recoverability / system-directory probe、破壊的 call、shell pass）は変わらない。
  **RESIDUAL**: 書き込み call の検出（`interpreter_write_call`）は文字列の部分一致のままなので、string 内に
  write call の綴りがあり、別の literal が path のとき、その path は layer 0 で probe される（制限側。
  コード読解による記述で、この形を突くテストは未作成）。
- **`cat`/`tee` が here-string（`<<<`）をファイルへ書くとき、その operand をファイルの内容として shell 規則で
  判定する（tests/heredoc_system_harm_holes_20261111.rs E、`detect/here_string_file.rs`）** — here-document
  の本文と同じ target 区分: 全 target が data file・`/dev/null` なら判定しない。全 target が `.py` または
  非 shell の code file（`.rs` 等）なら、command の他の segment がすべて `INERT_LATER_COMMANDS` で始まり
  `$`・backtick・`\`・括弧・波括弧・`<`・`>`・`#`・不均衡な quote を含まないときだけ判定しない。それ以外
  （`.sh`・拡張子なし・未知の拡張子・literal でない target）は常に判定する。判定は `analyze_shell_payload`
  （Deny > Ask > Allow）。書く shell が展開・unquote する operand（`$`/backtick を含む double-quoted か bare、
  quote/`\` を含む bare）は内容が読めないので Ask。`cat > x.rs <<< 'rm -rf ~' && sh x.rs` は Deny。
- **`echo`/`printf` がファイルへ書いた literal を、同じ command の別の部分がそのファイルを実行しうるとき shell
  script として判定する（tests/script_write_then_run_harm_20261111.rs H1–H3、`detect/echo_script_file.rs`。
  `payload::analyze_interpreter_stdin_exec` の先頭で呼ぶ）** — writer は command 語（exec-wrapper の後ろも
  `command_candidates` で見る）が `echo`/`printf` の pipe stage で、stdout を `>`・`>>`・`>|`・`&>`・`1>`
  （語に glue されていても）で、または直後の `| tee FILE…` / `| cat > FILE` stage でファイルへ書くもの。
  writer は予約語の後ろ（`do echo …`・`then echo …`・`{ echo …`）や、subshell 語・shell が実行する
  substitution の内側（`(echo … > x.sh)`・`$(echo … > x.sh)`）にあってもよい（tests/echo_script_variants_20261111.rs
  F2・F6）。同じファイル（先頭 `./` は無視）への書き込みは command の順に積み上げ（`>` は置換、`>>` は追記）、
  書き込みごとの内容を**すべて**判定する。後の上書きで前の内容を落とさない: 上書きは失敗しうる（呼び出し元から
  継承した `noclobber`・`set -C`・`chmod` 等。user ruling 2026-11-11「Back to Deny」、V16
  `echo 'rm -rf ~' > x.sh; echo 'echo hi' > x.sh; sh x.sh` は Deny）。実行の後に書かれた内容も判定する
  （過剰側。`…; sh x.sh; echo hi > x.sh` は Deny）。`echo` の出力は shell ごとの読み方をすべて追う
  （user ruling 2026-11-11「Deny」、V13）: bash（`-n`/`-e`/`-E` を option とし、escape は `-e` のときだけ展開）、
  zsh・bash `xpg_echo`（同じ option、escape は `-E` でない限り展開）、dash/POSIX `sh`（先頭の `-n` だけが
  option、escape は常に展開）。読み方ごとに判定し、どれかが Deny なら Deny、Deny が無く再現できない読み方が
  あれば Ask（`echo -e 'rm -rf ~\n' > x.sh && sh x.sh` は Deny）。展開するのは `\\`・`\a`・`\b`・`\c`
  （以降を出力せず改行も出さない）・`\f`・`\n`・`\r`・`\t`・`\v` で、他の escape を展開する読み方は再現不能。
  `>>` は前の各読み方と新しい各読み方を連結し、16 を超えた分は再現不能な読み方 1 つ（Ask）に置き換える。
  target 区分（data file 等）では除外しない。判定するのは別の stage がその
  ファイルを実行しうるときだけ: command 語がそのファイルを指す stage（`./x.sh`）、またはそのファイルを指す語と
  shell・`.`・`source` の語をどこかに持つ pipeline（`sh x.sh`・`. x.sh`・`sh < x.sh`・`cat x.sh | sh`）。
  そのファイルを指す語と `python`/`python3`/`python3.N` の語を持つ pipeline は、そのファイルを Python program
  として、ほかの code interpreter（`is_code_interpreter`: `node`・`perl`・`ruby` 等）の語を持つ pipeline は
  その interpreter の program として実行しうるとみなす。subshell 語（`(sh x.sh)`）と shell が実行する substitution（`$(…)`・backtick・
  `<(…)`/`>(…)`）の本体も、それぞれ独立した statement として同じように探す
  （tests/echo_script_variants_20261111.rs V9）。
  「指す」は literal 値の basename 一致、展開を含む語は basename の部分一致か glob 文字を含むこと。literal で
  ない書き込み先は、shell・`.`・`source` の語か `$`/backtick を含む command 語を持つ pipeline があれば shell
  script として、python・ほかの code interpreter の語を持つ pipeline があればその program として実行されうる
  とみなす。判定は、shell の実行には `analyze_shell_payload`、Python の実行には `.py`
  here-document と同じ Python judge（`interpreter::python_written_program_verdict`。full verdict を使う。
  command のどこかに `cd`/`pushd`/`popd`/`-C`/`--chdir` の語があれば相対 path を置き場所不明として判定）、
  ほかの code interpreter の実行には、その interpreter の inline-eval flag（`node -e '<内容>'`）と同じ program
  監査 `interpreter_code_verdict`（JS 専用の judge は無い。破壊的呼び出し一覧に `rmSync` 等があるので、
  `echo "require('fs').rmSync(process.env.HOME,{recursive:true})" > x.js && node x.js` は Deny、F7。
  対象 path によらず Deny になるのは `node -e` と同じ）（Deny > Ask > Allow）。
  `echo "import shutil; shutil.rmtree('/etc')" > x.py && python3 x.py` は Deny（V15）。再現できない内容は Ask: `$`・
  backtick・`\`（single quote 内を除く）・glob/brace 文字・bare の演算子文字・先頭以外の `~` を含む引数（先頭の `~`/`~/` は `~` のまま
  読む）、上記以外の escape を展開する `echo` の読み方、`%%` 以外の `%` 変換・`\n`/`\t`/
  `\\` 以外の escape・`-` で始まる format・format 後の引数を持つ `printf`。`echo 'rm -rf ~' > x.rs && sh x.rs`
  は Deny、`echo 'echo hi' > x.sh && sh x.sh` と `echo 'hello' > x.txt && cat x.txt` は Allow。
  **RESIDUAL**（コード読解による記述。これらの形を突くテストは未作成）: 実行が別の Bash 呼び出しにある場合、
  `>>` が追記する前の既存内容、別種の writer（here-document・here-string・`cp`）と `echo` の書き込みの連結、
  `is_code_interpreter` に無い interpreter（`tsx`・`Rscript` 等）が実行するファイルは、この判定の外。
  code interpreter の program 監査は `node -e` と同じ文字列ベースの破壊的呼び出し検出であり、言語ごとの
  parser ではない。`>>` の連結で読み方が 16 を超えると、超えた分の読み方は判定せず Ask にする（そこに system
  harm があっても Deny ではなく Ask）。
- **未修正の false positive の RED（2026-11-13、flow-8cca7ef5。修正は未実装）** — `.rs` 宛ての quoted heredoc の
  body は data であり shell ではない（ユーザー裁定: heredoc body を .py/.rs へ書くコマンドの body は data、
  .sh/拡張子なしだけ shell として判定、同一 command 内で実行される場合を除く）。しかし (FP1) body に Rust 文字列
  リテラルとして `python3 -c \"...\"` を含む複数行 body + 後続の `cd && cargo test` は「python3 の inline program を
  tokenise できない」で Ask（subagent では Deny）、(FP2b) body に `git add && git commit -m \"a \\\"b\\\" c\" -m ...`
  のテキストを含む body + 後続コマンドは「whole-tree staging（unclosed double quote）」で Ask/Deny になる。報告された
  素の `git add <paths> && git commit -m "..\"..."` コマンド自体は Allow で再現しなかった（control）。
  回帰テスト: `crates/blastguard/tests/backlog_20261113_fp_heredoc_rs_body_and_commit_msg_quotes.rs`
  （`#[ignore]` の RED 2 本 + control。独立 subagent が書いた。証明しない範囲はファイル冒頭の doc comment）。
- **system-harm fail-open の RED（2026-11-13、flow-8cca7ef5。(a) は backlog 00569f7d・blastguard 0.2.530、(b) は backlog 6df87a5c・blastguard 0.2.537 で修正済み。`#[ignore]` は外した）** — shell に渡す program の
  本文が実行時に作られる 2 形は、system harm でも Allow になっていた: (a) `printf` の escape 展開
  （`printf 'chm\x6fd 777 /etc/hosts' | bash`、octal `\157` も同じ）、(b) process substitution から stdin で読む
  `bash < <(echo 'chmod 777 /etc/hosts')`。literal の `printf 'chmod 777 /etc/hosts' | bash` は Deny（control）。
  回帰テスト: `crates/blastguard/tests/backlog_20261113_printf_escape_and_procsub_to_shell.rs`
  （RED 2 本 + control。orchestrator が書いた。証明しない範囲はファイル冒頭の doc comment）。
- **system-harm fail-open の RED（2026-11-13、flow-8cca7ef5。backlog 5616e9e5 で修正済み〈main の f6dbff5b の修正で GREEN〉。`#[ignore]` は外した）** — Python の `os.chmod` /
  `os.rename` の path 引数を文字列 literal の escape（`\x2f`、octal `\057`、`\u002f`、`\N{SOLIDUS}`）で綴った
  system パスは Allow になっていた（main・subagent とも）。escape を使わない `'/etc/hosts'` は Deny（control）。
  回帰テスト: `crates/blastguard/tests/backlog_20261113_python_escape_decoded_os_path_call.rs`
  （RED 1 本 + control。orchestrator が書いた。証明しない範囲はファイル冒頭の doc comment）。
- **未修正の false positive の RED（2026-11-14、flow-8cca7ef5。修正は未実装）** — python の
  `shutil.copy2(SRC, DST)` は読むだけの SRC も書き込み先として判定される: `shutil.copy2('/etc/hosts','/tmp/x')` は
  「it writes `/etc/hosts`」で Deny、SRC だけを `/tmp/y` に変えた同形は Ask（main session）。
  回帰テスト: `crates/blastguard/tests/backlog_20261114_shutil_copy2_source_judged_as_write_fp.rs`
  （`#[ignore]` の RED 1 本 + control。orchestrator が書いた。証明しない範囲はファイル冒頭の doc comment）。
- **system-harm fail-open の RED（2026-11-14、flow-8cca7ef5。keyword 引数・alias import の 2 形は backlog f6dbff5b、
  残り 3 形は backlog 17d985f1・blastguard 0.2.526 で修正済み。`#[ignore]` は外した）** — python の書き込み API で、
  system パスの書き込み先が呼び出しの単一 literal 引数でない形は Allow になっていた（main・subagent とも）:
  `(Path('/etc') / 'hosts').write_text(..)`、`Path('/etc').joinpath('hosts').write_text(..)`、
  `shutil.copy(dst='/etc/hosts', src=..)`（keyword 引数）、`from shutil import copy as c; c(.., '/etc/hosts')`（alias import）、
  `Path('/tmp/y').replace('/etc/hosts')`（移動先の operand）。`os.chmod('/etc/hosts',0)` は Deny（control）。
  回帰テスト: `crates/blastguard/tests/backlog_20261114_python_write_api_indirect_system_target.rs`
  （RED 1 本 + control。orchestrator が書いた。証明しない範囲はファイル冒頭の doc comment）。
- **comment collapse の回帰ガード（2026-11-14、spec blastguard-comment-collapse-residual R1 の旧 R3）** — `#` コメント行の
  次の行で `os.chmod('/etc/hosts',0)` を呼ぶ python program を、解決できないコマンド語（`$PY -c` / `"$PY" -c`）、
  `find -exec sh -c`、ANSI-C `$'..'` で渡す形は、main・subagent とも Allow にならない（a473ba71 で観測済み。ignore なし）。
  回帰テスト: `crates/blastguard/tests/comment_collapse_r3_shapes_stay_denied_20261114.rs`
  （e4523bfd の実装者ではなく orchestrator が書いた。検出力の証跡はファイル冒頭の doc comment と commit message）。
- **未修正の false positive の RED（2026-11-14、flow-8cca7ef5。修正は未実装）** — `printf` の二重引用符 operand が
  `\$PY -c, \\\"\$PY\\\" -c, find -exec sh -c ... os.chmod on /etc/hosts ...` のような文字列だと、印字するだけの
  `printf` が「`printf` is not a command blastguard recognises, and what follows it parses as a destructive command line」で
  Ask（subagent では Deny）になる。`\$PY ...` の部分を除いた同形は Allow（control）。
  回帰テスト: `crates/blastguard/tests/backlog_20261114_printf_operand_judged_as_command_fp.rs`
  （`#[ignore]` の RED 1 本 + control。orchestrator が書いた。証明しない範囲はファイル冒頭の doc comment）。
- **未修正の system-harm fail-open の RED（2026-11-14、subshell / brace group / source reader への pipe。修正は未実装）** —
  `printf 'chmod 777 /etc/hosts' | (bash)`・`| { bash; }`・`| . /dev/stdin`・`| source /dev/stdin`、および `echo ... | (bash)`・
  `echo ... | { bash; }` は main・subagent のどちらでも Allow（0.2.539 で再観測。独立 verifier が 0.2.530 / main 144906a1 で観測）。
  素の `| bash` は Deny（control）。system harm は常に Deny であるべき。
  回帰テスト: `crates/blastguard/tests/backlog_20261114_pipe_into_subshell_brace_source_reader.rs`
  （`#[ignore]` の RED 1 本 + control。独立 test author が書いた。証明しない範囲はファイル冒頭の doc comment）。
- **未修正の false positive の RED（2026-11-14、printf で書いたファイルを python の data 引数に渡す形。修正は未実装）** —
  `printf '%s\n' a b > d.txt && python3 s.py d.txt` が、`d.txt` を python の program として判定して Ask（subagent では Deny）になる
  （printf の format-reuse が未モデルのため）。`d.txt` は `s.py` の argv であって program ではない。単一 conversion の printf は Allow。
  `> s.py && python3 s.py`（ファイルが program）は non-Allow のまま（control）。
  回帰テスト: `crates/blastguard/tests/backlog_20261114_printf_file_as_python_data_arg_fp.rs`
  （`#[ignore]` の RED 1 本 + control。独立 test author が書いた。証明しない範囲はファイル冒頭の doc comment）。
- **未修正の system-harm fail-open の RED（2026-11-14、value-taking flag / `-s` 引数付き shell への pipe。修正は未実装）** —
  system file（/etc 配下の hosts）を world-writable にする program を `echo` で `bash -o pipefail` / `bash -s a b` /
  `bash -O extglob` / `bash --rcfile f` / `sh -s a b` / `sh -o pipefail` に pipe すると main・subagent とも Allow になる
  （flag の値や `-s` 以降の位置引数を script ファイルと見なして stdin を検査しない疑い。未検証）。素の `| bash`・`| bash -e`・
  `| bash -s` は Deny のまま（control）。
  回帰テスト: `crates/blastguard/tests/backlog_20261114_pipe_to_shell_with_value_flags_or_s_args.rs`
  （`#[ignore]` の RED 1 本 + control。独立 test author が書いた。証明しない範囲はファイル冒頭の doc comment）。
- **未修正の system-harm fail-open の RED（2026-11-14、compound command 内 / fd 3 / zsh `=()` / subshell 内 pipe で shell に渡る program。修正は未実装）** —
  system file（/etc 配下の hosts）を world-writable にする program を、process substitution 経由で shell に渡す形が
  subshell `( )`・brace group・`if` / `while` / `for` の本体に入っていると main・subagent とも Allow になる。同じく fd 3 に
  process substitution を開いて次の文で shell が fd 3 を stdin として読む形、`zsh =( ... )`、subshell 内の pipe 先 shell も Allow
  （この main 由来の build では素の process substitution 形も Allow。0.2.541 で観測、独立 verifier は 0e726e66 / 572cc1c3 で観測）。
  素の pipe-to-shell と subshell 内の直接実行は Deny のまま（control）。system harm は常に Deny であるべき。
  回帰テスト: `crates/blastguard/tests/backlog_20261114_compound_wrapped_or_cross_stage_shell_program.rs`
  （`#[ignore]` の RED 1 本 + control。独立 test author が書いた。証明しない範囲はファイル冒頭の doc comment）。
- **未修正の false positive の RED（2026-11-14、shell `-c` の literal program 内の `echo - foo`。修正は未実装）** —
  `zsh -c 'echo - foo'` / `bash -c 'echo - foo'` が「program は stdin から取る」と誤判定されて Ask（subagent では Deny）になる。
  program は command line 上にある。`echo -- x` / `echo -nE a` / `echo -x y` は Allow で、引き金は単独の `-` operand。
  harmless な変種は Allow、harmful な `zsh -c` program（system hosts file を world-writable にする）は Deny のまま（control）。
  回帰テスト: `crates/blastguard/tests/backlog_20261114_zsh_c_literal_echo_dash_fp.rs`
  （`#[ignore]` の RED 1 本 + control。独立 test author が書いた。証明しない範囲はファイル冒頭の doc comment）。
- **RED（2026-11-14、flow-8cca7ef5。backlog 6df87a5c・blastguard 0.2.533 で修正済み。`#[ignore]` は外した）: procsub operand と list の twin** — (1) system-harm
  fail-open: シェルに渡す process substitution が先頭 operand でないとき（`bash <(echo '…') foo` の script 引数、
  `bash -x` / `-o pipefail` / `-O extglob` / `--rcfile f` の値取りフラグ、`bash <&3 3< <(…)` の fd dup、
  `source <(…) foo`）、`/etc/hosts` の re-permission を走らせるのに main / subagent とも Allow。system harm は常に Deny。
  (2) false positive: `bash <(echo hi; echo there)` / `bash < <(echo hi; echo there)` は静的に無害と証明できる list だが
  Ask（subagent では Deny）。base（0e726e66）では Allow だった。control: 先頭 operand の `bash <(echo '…')` と
  `bash 0< <(…)` は Deny、`bash < <(echo hi)` / `diff <(sort a) <(sort b)` / `bash script.sh < <(echo y)` は Allow、
  `bash < <(cat f)` は Allow ではない。
  回帰テスト: `crates/blastguard/tests/backlog_20261114_procsub_operand_flags_and_list_twins.rs`
  （RED 2 本 + control。独立 author が書いた。証明しない範囲はファイル冒頭の doc comment）。
- **false positive の RED（2026-11-14、t2 実装者の観測。backlog 6df87a5c・blastguard 0.2.535 で修正済み。`#[ignore]` は外した）: procsub list の出力に引用符** —
  `bash <(echo "it's"; echo hi)` / `bash < <(echo "it's"; echo hi)` は 2 つの literal echo だけで出力が静的に
  証明でき無害だが、結合出力に `'` を含むことだけを理由に Ask（subagent では Deny）。base（0e726e66）では Allow だった。
  control: 同じ引用符入り list の 2 つ目の echo が system file の re-permission を出力する形は main / subagent とも Deny のまま。
  回帰テスト: `crates/blastguard/tests/backlog_20261114_procsub_list_quote_output_fp.rs`
  （RED 1 本 + control。独立 author が書いた。証明しない範囲はファイル冒頭の doc comment）。
- **RED（2026-11-14、orchestrator の観測。backlog 6df87a5c・blastguard 0.2.537 で修正済み。`#[ignore]` は外した）: procsub を `--rcfile` / `--init-file` に渡す** —
  interactive bash は `--rcfile` / `--init-file` の引数を program として source する。その値が process substitution で、
  出力が `/etc/hosts` の re-permission のとき（`bash --init-file <(…) -i` / `bash --rcfile <(…) -i` /
  `bash --rcfile <(…)` / `bash -i --rcfile <(…)` / `bash --rcfile=<(…) -i`）、実行されるのに main / subagent とも Allow。
  procsub 側が option の値を判定せず消費しているため。system harm は常に Deny。
  control: 無害な `bash --rcfile <(echo hi) -i` / `bash --init-file <(echo hi) -i` は Allow のまま。
  回帰テスト: `crates/blastguard/tests/backlog_20261114_procsub_rcfile_init_file.rs`
  （RED 1 本 + control。独立 author が書いた。証明しない範囲はファイル冒頭の doc comment）。
- **CR で終わる python コメントの system-harm fail-open（2026-11-14、flow-8cca7ef5、backlog a66c115c で修正済み）** —
  CPython は program を字句解析する前に `\r\n` と裸の CR（`\r`）を `\n` に変換する（3.12.11 で `python3 -c`・stdin・
  ファイル・`compile` のいずれでも観測: CR は `#` コメントを終え、文を区切り、`\` の行継続になり、三重引用符内では値 `\n`、
  一重引用符内では unterminated string の SyntaxError。form feed・`\x0b`・U+2028 はコメントを終えない）。以前の blastguard は
  `\n` でしか行を終えず、`python3 -c "import os # x<CR>os.chmod('/etc/hosts',0)"`（`env -S` 経由・here-string・
  `python3 - <<'EOF'`・`cat > x.py <<'EOF'` 後の実行も同じ）の chmod をコメントの一部と読んで Allow にしていた。
  0.2.535 以降、Python が読む program のすべての入口（`interpreter_code_verdict` の Python reader、`python_program_verdict`・
  `python_stdin_body_reading`・`python_strings_never_reach_a_shell`・`python_program_tokenises`・`python_redirect_view`）で
  同じ変換（`src/detect/interpreter/python_file.rs` の `python_newlines`）をしてから読むので、main・subagent とも Deny になる。
  一重引用符内に CR がある program は LF の同形と同じく「字句解析できない」Ask（subagent では Deny）になる。
  ruby・perl・sh の `#` コメントは CR で終わらない（観測）ので、それらの読みは変えていない。
  LF の同形は Allow にならない（control）。
  回帰テスト: `crates/blastguard/tests/backlog_20261114_python_cr_ends_comment.rs`
  （RED 1 本 + control。orchestrator が書いた。証明しない範囲はファイル冒頭の doc comment）。
- **system-harm fail-open の RED（2026-11-14、flow-8cca7ef5、pathlib のメソッド参照。backlog c8bf27ea・
  blastguard 0.2.542 で修正済み。`#[ignore]` は外した）** —
  pathlib の書き込み・移動メソッドを直接呼び出し `Path(..).method(..)` 以外で呼ぶ形は Allow になっていた（main・subagent とも）:
  unbound 呼び出し `Path.replace(Path('/tmp/z'), '/etc/hosts')`、束縛メソッドを名前に入れて呼ぶ
  `w = Path('/etc/hosts').write_text; w('x')`、`Path('/etc/hosts').write_text.__call__('x')`、
  round 2 として class 属性に入れた参照 `class C:` 本体の `w = Path('/etc/hosts').write_text` → `C.w('x')`。
  直接呼び出し `Path('/etc/hosts').write_text('x')` は Allow にならない（control）。修正の内容は
  「filesystem 破壊」項の **メソッド参照**（`interpreter/python_semantics/method_refs.rs`）を参照。
  回帰テスト: `crates/blastguard/tests/backlog_20261114_python_write_method_reference.rs`
  （RED 2 本 + control。orchestrator が書いた。証明しない範囲はファイル冒頭の doc comment）。
- **未修正の false positive の RED（2026-11-14、flow-8cca7ef5、単一引用符内の perl 特殊変数。修正は未実装）** —
  単一引用符の `perl -ne 'print if $.==9' /dev/null` は shell が `$.` を展開しないのでコマンド行の文字列がそのままプログラムだが、
  blastguard は「inline-eval のプログラムが shell 展開を含む」として拒否する（subagent で deny を観測）。
  二重引用符で本当に展開される `perl -ne "print $HOME"` は Allow にならない（control）。
  回帰テスト: `crates/blastguard/tests/backlog_20261114_perl_single_quoted_dollar_fp.rs`
  （`#[ignore]` の RED 1 本 + control。orchestrator が書いた。証明しない範囲はファイル冒頭の doc comment）。
- **未修正の false positive の RED（2026-11-14。修正は未実装）** — 引用付き heredoc で `.rs` ファイルへ書く本文に
  Rust の match arm `=>`（例 `b'f' => format!("{v:.p$}"),`）があり、同じコマンド内に後続の `python3 - <<'EOF'` heredoc 文が
  あると、`>` が redirect と判定され「destroys the file's current contents and ... contains a shell expansion」で Deny になる。
  `.rs` heredoc 単体は Allow。実際の redirect（`: > /etc/hosts`）と、書いて実行する heredoc（`sh x.sh`）は Deny のまま（control）。
  回帰テスト: `crates/blastguard/tests/backlog_20261114_heredoc_rs_body_match_arrow_fp.rs`
  （`#[ignore]` の RED 1 本 + control。独立 test author が書いた。証明しない範囲はファイル冒頭の doc comment）。

### module 責務

- **`main`** — CLI エントリ。version/help 短絡 → `hook::run_hook(run)`。`run` が stdin parse →
  `detect` → deny 時のみ `deny_json` を print。ターン非破壊契約の実装点。
- **`detect`** — 純粋な破壊操作検出器。`detect`（tool 名で dispatch）、`detect_write`（`detect/file_tool`）、`detect_bash`、
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
- **`model`** — 判定型 `Decision::{Allow, Deny(String), Ask(String)}`（`deny()`/`ask()`/`is_deny()`）。
  system-harm の Deny は `deny_system_harm()` で理由に `SYSTEM_HARM_MARK` を付け、`is_system_harm()` で識別する。
- **`detect/verdict_acc`** — `detect` の再解析ループ（segment・`-c` payload・command-word 候補・`-exec` tail）が
  共有する `VerdictAcc`。`Deny > Ask > Allow` の順位で畳み、`Deny` は即座に返し、`Ask` は最初の 1 件を保持して
  走査を続ける（後続の `Deny` を `Ask` が隠さない）。順位は `harness_core::verdict::Verdict::worst_of` そのもので、
  `Decision`↔`Verdict` の変換は境界の 2 関数だけが行う。
- **`detect/file_tool`** — file tool（Write / Edit / MultiEdit / NotebookEdit）の判定。`extract_path`
  （`file_path`/`notebook_path`/`path`）・`detect_write`・`detect_edit`・`file_tool_system_block`。
  `detect.rs` から挙動を変えずに移した（file-growth cap）。
- **`detect/budget`** — `detect_bash` の再帰の 2 つの上限。深さ上限 `MAX_SHELL_DEPTH` に達したら
  `depth_exhausted`（Ask: 解析未完了）、1 コマンドあたりの node 訪問予算 `MAX_ANALYSIS_NODES` を
  `ANALYSIS_BUDGET` から `take_budget` で消費する。`detect.rs` から挙動を変えずに移した（file-growth cap）。
- **`detect/tests/file_ops`** — `detect/tests` の子 module。file tool（Write / Edit / MultiEdit /
  NotebookEdit）の unit test と、entry-boundary の「読めない operand は Ask、管轄外 tool は Allow」test。
  `detect/tests.rs` から変えずに移した（file-growth cap）。
- **`scope/deletion_roots`** — `SafeRoots` の temp / cache DELETION root（spec 3aa215e1 class 2/3）を組み立てる
  `deletion_roots` と `DELETION_TEMP_ROOTS`・`DELETION_CACHE_ROOTS`・`OS_TEMP_PARENTS` および除外判定
  （`contains_home`・`below_home`・`system_root`）。`scope.rs` から挙動を変えずに移した（file-growth cap）。
- **`detect/condukt`** — 自律の付与は人間だけが決める（backlog 12ba5845）。`condukt state autonomy-set on` と、
  `condukt` 呼び出しに前置された `HARNESS_AUTONOMOUS=…` / `CONDUKT_AUTONOMOUS=…` 代入（`env` 経由も含む）を
  `Ask` にする。subagent からの payload では `main.rs` の `harden_for_subagent` が `Deny` に固める。照合は
  parse 済みの command word と argv token だけで行い、生のコマンド文字列は見ない（`echo "condukt …"` は対象外）。
  前の文での `export CONDUKT_AUTONOMOUS=1; condukt …` は見ない。
- **`log`** — `blastguard log`: overwatch の violations ledger（`overwatch::store::violations_path`）から
  `source == "blastguard"` の行だけを file 順に列挙する read-only の照会コマンド。stdin を読まず ledger へ書かない。
  フィルタは `--decision deny|ask`・`--rule`・`--since`（RFC3339 または期間）・`--system-harm`・
  `--subagent` / `--interactive`（排他）、`--json` で JSON 出力。旧形式の行に無いフィールドは UNKNOWN
  （text `unknown` / JSON `null`）で、そのフィールドへのフィルタには一致せず除外件数を stderr に出す。
  text 出力は C0 制御文字・DEL・backslash を escape し、C1 制御（U+0080〜U+009F）・U+2028/U+2029・
  bidi 制御（U+061C / U+200E / U+200F / U+202A〜U+202E / U+2066〜U+2069）を `\u{XXXX}` に escape して
  1 entry = 1 行と表示順の不変を保証するが、曖昧さは無くさない（値は quote されない）。厳密な解析は `--json` を使う。
  exit: 0 = 成功（一致なし・ledger 不在を含み、不在は stderr に `no ledger at <path>`）、2 = 使い方の誤り、
  3 = ledger を特定・読めない／不正な行がある（有効な entry は出力したうえで `line <N>`）／stdout 書き込み失敗。
- **`relay`** — subagent の Ask を main session で裁定させる relay（ユーザー裁定 2026-11-14 逐語:
  「sub agentのAskはmainで裁定する」「この仕組みはコードで強制する、どうせわすれる」
  「gateにそんな例外は追加してほしくない」「gateもconduktも絶対に勝手に他のworktreeを参照してはならない」）。
  session ごとの append-only JSONL queue（`~/.blastguard/relay/<session_id>.jsonl`、`BLASTGUARD_RELAY_DIR`
  で上書き。session_id が `[A-Za-z0-9_-]` 以外を含めば hash してファイル名にし dir 外を指させない）に
  `queued` / `executed` / `relayed` / `dismissed` event を追記し、fold した状態で判定する。
  (1) `main` の `emit` が subagent の **Bash の `Ask`** を `Deny` に硬化するとき、`ask_available()` が真
  （main session が ask できる）なら `queued`（id `bgr-<12 hex>`・ts・session_id・agent_id・cwd・command・
  Ask 理由）を追記し、id を**表示する** deny 理由の末尾に付ける（violation ledger へは付加前の理由を
  記録するので `rule_id` は動かない）。`Deny`（system harm を含む）・main の `Ask`・headless の subagent
  `Ask` は記録しない。書き込み失敗・panic でも判定は `Deny` のままで、失敗を deny 理由に明記する。
  (2) `relay mark-executed`（PostToolUse / PostToolUseFailure、Bash）: main session（`payload_is_subagent`
  が偽）の呼び出しで command と cwd がバイト一致する最古の未実行・未 dismiss entry に `executed`
  （`tool_response` の整数 `exit_code`/`exitCode`/`returnCode`/`code`、無ければ `error` の `Exit code N`、
  無ければ null）を追記。(3) `relay mark-relayed`（PostToolUse、SendMessage）: main session の
  `tool_input` 全体に id を含む**実行済み**の entry に `relayed` を追記。(4) `relay stop`（Stop、
  `run_guarded("blastguard-relay")` 下）: 未解決（executed∧relayed でも dismissed でもない）entry が
  あれば `stop_hook_active` に関わらず block し、id・agent_id・cwd・command と手順を列挙。queue 読み取り
  失敗・パース不能行・未知 id の event・パース不能な Stop payload は block（path を明示）。queue 不在は
  allow。dismiss 済みは `systemMessage`（pending があれば block 理由）に毎回列挙。(5) `relay dismiss
  <id> --reason <text>`: 空白のみ/欠落の理由・未知 id は exit 2、queue 読み書き失敗は exit 3。
  コマンドを実行する経路は持たず、cwd は不透明な文字列として比較するだけで解決・stat しない。
- **`blocklog`** — block log（ユーザー裁定 2026-11-14 逐語:「…完全なコマンドが残っていないため、
  これ以上は切り分けられません。というのは大変愚かなので記録をとること。止めたなら止めた責任をとる」）。
  表示した `Deny` / `Ask` ごとに 1 行を overwatch に依存せず `~/.blastguard/blocks/blocks.jsonl`
  （`BLASTGUARD_BLOCK_LOG_DIR` で dir を上書き）へ追記する。`Allow` は記録しない。フィールド:
  `v`(2)・`ts`・`blastguard_version`・`session_id`・`agent_id`（subagent 外は null）・`cwd`（payload 値、
  不透明・非検査）・`tool_name`・`command`（Bash、バイト一致・非切り詰め）・`file_path`・`tool_input`
  （payload の値そのもの）・`tool_input_capped`・`tool_input_bytes`・`tool_input_sha256`（compact JSON の
  sha256）・`decision`（表示した `deny`/`ask`）・`decision_kind`（`deny` / `ask` / `ask-hardened-to-deny`
  〈subagent・headless での硬化〉/ `deny-downgraded-to-ask`〈2 度目〉/ `panic-blocked`〈verdict 経路の
  panic で exit 2〉）・`original_decision`（検出器の元の判定。`deny` / `ask` / `undetermined`〈panic
  で判定に至らず〉。全 `decision_kind` で必ず出力し null にならない。`deny-downgraded-to-ask` は元が
  headless で硬化した Ask なら `ask`、真の Deny なら `deny`。欠落・null を `deny` と読まない — v1
  レコードには無い）・`actor`・`system_harm`・`reason`（表示文そのもの）・`rule_id`・`raw_payload`
  （payload をパースできなかったとき・panic 時のみ raw stdin）。`tool_input` の複製だけは compact JSON が
  1 MiB を超えると null・`tool_input_capped: true`（長さ・hash・`command`・`file_path` は保持）。
  記録は判定確定後・表示前に書き、判定を変えない。書けない（panic を含む）ときは表示理由に
  `could NOT be recorded` とエラーを付け stderr にも警告する（沈黙しない）。relay（subagent の Bash Ask の
  再実行 queue）とは別物で、block log は真の `Deny` と main の `Ask` を含む全 block を記録する。
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

## exec が開く fd の文をまたいだ追跡 — ユーザー裁定 2026-11-14（backlog e686a85e）

裁定（AskUserQuestion への回答、2026-11-14）:

1. **中身が system harm と literal で分かる program は、形を解析できなくても main・subagent ともに Deny。**
   中身に harm を含む literal の program text が、blastguard のまだモデル化していない形で shell / interpreter に渡る場合も、main で Ask にせず Deny とする（「system harm はどの wrapper を通しても Deny」の適用）。
   中身が literal で読めない（判定不能な）場合は従来どおり Ask（subagent では Deny）で、Allow にはしない。
2. **追跡は最小範囲とする。**
   - モデル化する形: `exec N<<< literal` と、quoted here-document（`exec N<<'EOF'`）で開いた fd を、後の文の `bash|sh|zsh` または interpreter が `<&N` か `/dev/fd/N` で読む形。文は `;`、改行、`&&`、`||` で区切られ、書かれた順に追う。
   - `exec N<&-`（close）と、同じ N への再代入は、書かれた順に反映する。close した後と、再代入した後の fd は、新しい中身で判定する。
   - subshell `( … )`、関数本体、条件付きの exec（`&&` / `||` の右辺や `if` の中）、展開を含む here-document（unquoted `<<EOF`）、`N< file`、`N<&M` の連鎖は、追跡の対象外とする。これらの形で開いた fd を読む場合は判定不能として扱う。ただし中身が literal で harm と分かる場合は、裁定 1 により Deny とする。
3. 再現テスト: `crates/blastguard/tests/backlog_20261114_exec_fd_herestring_then_shell_reads_fd.rs`（`exec 3<<< 'chmod 777 /etc/hosts'; bash <&3` は main・subagent とも Deny）。
