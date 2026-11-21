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
- **判定は純粋・決定論** — `detect` / `exclude` は I/O を持たず、`(入力) -> 出力` の
  純関数。config allowlist は `exclude::glob_set()`（`OnceLock<GlobSet>`）でメモ化するのみ。
- **classify / diffrisk は riskgrade へ移設（2026-11-17）** — graded risk/reversibility 分類器と diff-level リスクは `docs/specs/riskgrade.md`。blastguard はコマンド安全性の判定だけを持ち、riskgrade が `detect::detect` を再利用する（lockstep 不変条件も riskgrade 側に移した）。
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
  `truncate`（S6-4 以降、`detect/known_allow` が Known Allow のときは Allow。下記「module 責務」）/`shred`/`dd of=`（S6-5 以降、同じく Known Allow なら Allow。`of=` は shell の quote 除去後の語で読む — `'of=/dev/sda'`・`\of=…` も `of=`、`of=` になりうる不定語〈`$X`・`of=$F`〉も `of=` 扱い〈`rm::dd_of_operands`、0.2.792〉）/`mkfs*`、再帰 `chmod`/`chown`（`-R`/`--recursive`）、
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
  `$(…)`・backtick・`<(…)` の本体は外側 shell の変数を継承するので、本体内の command（2 文目以降・
  `{ …; }`・入れ子 substitution を含む）の `$X` operand も、その substitution を含むセグメントから見た
  代入で解決する（`rm::known_path_harm_in_substitutions`、backlog e5a8fcc2:
  `L=/etc/hosts; x=$(echo a; rm -f $L)` は main でも Deny）。ただしそのセグメント内に `$X`/`${X…` 以外の
  綴りで名前が現れる（`X=…`・`export X`・`read X`・`for X in`・`unset X`・代入 prefix
  `X=/tmp/x y=$(…)`）、本体に `eval`/`source`/`.` がある、行が関数を定義している、本体が `cd`/`pushd`/`popd`
  する、のいずれかなら解決しない（Ask のまま。Deny を足すだけ）。
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
- **quote された option 語は unquoted の綴りとして読む（raw-token quoted-option fail-open, 2026-11-20, critical）** —
  legacy arm は空白分割した raw token を `starts_with("--output=")`・`== "-i"` 等で照合していたため、
  `sort '--output=/etc/passwd' x`・`sed \--in-place -e d /etc/hosts`・`git push "--force" origin main`・
  `flock '--command=rm -rf /' /tmp/l`・`base64 '-d' | sh` 等が Allow だった（`tests/raw_token_quoted_option_20261120.rs`
  の 55 RED）。arm ごとの patch ではなく、arm に渡す token 列を作る 3 箇所（`analyze_segment`・high-blast tier・
  pipe-egress の stage）で `detect/option_words.rs` の `OptionWords` を通す: shell word の先頭 token で、
  quote 除去後の文字が `-` で始まるものは、option 名（最初の `=` まで、無ければ語全体）を AST parser の dequote
  （`redirect::dequoted_redirect_word`）と同じ結果に dequote し、値（`=` 以降）は `=` の時点で開いていた quote で
  再度開いた元の綴りのまま残す（`'--command=rm` → `--command='rm`。値を dequote すると arm 側の quote 処理で
  二重に外れる）。operand（`-` で始まらない語）・前の語の途中の token（`-m "use '-f' here"`）・unquoted の option は
  バイト列を変えない。option 名が literal に決まらない語（quote 内の展開 `"-$x"`、未対応の ANSI-C escape
  `$'\x2di'`、`-x` の後の escape された空白）は **Ask**（判定不能は制限側）。先頭が展開の語（`"$OPT"`）は
  unquoted の `$OPT` と同じく従来どおり。単独の `-` と「`-` + 空白」で始まる語（`'- markdown の箇条書き'`）は
  option 名ではないのでバイト列を変えない（2026-11-20 測定 FP: `printf '%s\n' '- … rm -rf …' >> f` が Ask になった）。**RESIDUAL**: 名前の途中から始まる quote（`--out'put'=`・`-'i'`）と
  変数経由の option は対象外。
- **rm 以外のシステムファイル変更コマンドも system-harm Deny（backlog 3c750df2）** — `detect/system_paths.rs` の
  `system_mutation_verdict` が `analyze_command_at` の per-command arm より先に 1 箇所で判定する。対象は
  `unlink`/`rmdir` の operand、`mv` の source（`-t` 時は全 operand）、`sed -i`/`--in-place`（`gsed` も同じ）の入力ファイル
  （`-e`/`-f` が無いときの先頭 operand は script として除外。BSD `-i ''` の空 suffix はファイルではない）、
  `sort -o`/`--output` の出力ファイル（2026-11-20 raw-token fix: unquoted の `sort -o /etc/passwd x` も Allow だった）、
  `perl -i` の入力ファイル（`-e` が無いときの script ファイルは除外）、`chmod`/`chown`/`chgrp` の target、
  `rsync` の `--del*` 時の destination と `--remove-source-files` 時の source。これらが
  `scope::is_inside_system_dir`（rm と同じ判定。字句の絶対パスと親 symlink 解決の両方、wildcard/brace は
  literal prefix、safe root 内は対象外、`/var/folders`・`/dev/null` は対象外）に入れば
  `Decision::deny_system_harm`。operand は quote-aware に再分割し shell と同じく unquote する。先頭の
  `~`/`$HOME`/`${HOME}` は home に展開し、それ以外の `$`/backtick 展開（`$'…'` 含む）を含む対象 operand は
  **Ask**（判定不能は制限側。例: `chmod +x "$f"`・`mv "$a" b`）。ただし rm と同じく、同じ行のテキストが値を
  決める展開（前セグメントの literal 代入・literal 語の `$(echo …)`）は解決して判定し、システムパスなら
  system-harm Deny（`system_paths::known_mutation_harm`、`rm::known_path_harm` から呼ぶ。
  `X=/etc/passwd; unlink $X`・`X=/etc/hosts; mv $X /tmp/h`・`X=/etc/passwd; chmod 000 $X`）。下の
  「コマンド文面だけで値が決まる変数は静的に解決して判定する」の resolver が解決できる形は、この判定より前に
  literal へ置き換わるので literal と同じ判定になる（解決して system 外なら literal と同じ。以前の「Ask のまま」は
  backlog 3a950c6c で撤回）。`find -exec` の `{}` と rsync の remote
  operand（`host:/path`）は判定しない。base 不明の相対 operand は rm と同じく判定しない（他の rule は残る）。
- **コマンド文面だけで値が決まる変数は静的に解決して判定する（backlog 3a950c6c / 5ef2d51f、ユーザー裁定
  2026-11-14「静的に判定できない blastguard の実装がゴミなので、それをなおす」「必ず静的に判定し、判断する」）** —
  `detect/readings/static_bindings.rs` の `resolve_static_bindings` が、top-level の Bash コマンドに対して
  **すべての reading・すべての rule より前に 1 回だけ**走る共通 resolver。文（`;`・`&&`・`||`・改行で区切られた
  segment）を順に走査して literal の束縛環境を作り、後の文の素の参照（`$X`・`${X}`・`"$X"`、連結
  `"$T"bin/tdd`・`${W}/a`・`$T/bin`）をその literal に置き換える。sed -i / unlink / mv 等の対象・command word・
  redirect 先・rm operand・cd はすべて置換後のテキストを判定するので、解決できた値は literal と同じ判定になり、
  「cannot resolve」「only exists at run time」「cannot tell」を理由に判定不能にはならない
  （`export F=/etc/hosts; sed -i '' 's/a/b/' $F` は system-harm Deny）。
  - 束縛として採るもの: 親 shell で（pipeline の段・`&` でなく）無条件に（`assignment_execution_is_unconditional`）
    実行される、代入だけの文（`X=lit`・`X="lit"`・`X='lit'`）と `export X=lit`（operand が `NAME=value` か
    素の名前だけ。flag 付きは不採用）。値は先に置換するので既知の名前からの連結（`B=$A/x`）も解ける。
  - 値として採るのは、空でなく `[A-Za-z0-9/._+,:@%^-]` だけから成る literal（空白・引用符・`$`・backtick・
    backslash・glob・brace・`~`・`=`・`#`・演算子を含まない）。置換が単語分割・glob・tilde・brace 展開・
    segment 分割を変えないことをこの文字種で保証する。参照を含む `"…"` で、全参照を置換した結果がこの文字種だけに
    なるもの（`"$T"`・`"$P/fstab"`）は引用符ごと置換し、literal の綴りと同じテキストにする（未解決の参照・glob・
    空白などが残る `"…"` は引用符を残す）。解決できる変数の後ろの glob（`P=/abs; echo hi > $P/*.log`）は
    literal と同じに判定する（ユーザー裁定 2026-11-15）。
  - 判定不能（置換しない = 従来どおり制限側）のまま残すもの: `$(cmd)`・backtick の値とそれによる再代入、
    未代入、`&&`/`||` や `if`/ループ/`case`/subshell/brace group の中の代入、pipeline・`&` の代入、配列、`+=`、
    その名前を素の参照以外で言及する他の文（`read X`・`for X in`・`unset X`・`printf -v X`・`(X=…`・
    `$(X=…)`。以後その名前は未知）、ループ内のどこかで再束縛される名前（ループ開始以降）、`eval`/`source`/`.`
    （環境を捨てる）、shell が維持する名前（`PWD`・`RANDOM`・`_` 等）、here-document の body と閉じない
    opener 以降。
  - 行全体を解決しない（文面のまま判定）もの: shell 関数を定義する行、`IFS` を含む行、`trap`/`alias`/
    `readonly`/`declare`/`typeset`/`local` を含む行、`#` コメントか `$'…'` を含む行、segment をテキストへ
    対応付けられない行。
  - 対象外: `sh -c` 等の payload 内部の束縛（payload の再解析には適用しない）、プロセス環境由来の値。
  回帰テスト: `crates/blastguard/tests/backlog_20261114_static_literal_var_resolution.rs`・
  `crates/blastguard/tests/backlog_20261114_sed_inplace_literal_var_path_fp.rs`。
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
- **pid `-1` への `kill` は Deny（`detect/kill.rs`、rule id `kill-all`、backlog e93d3701 / spec 596ef57d Q5）**:
  `kill -9 -1` はユーザーが signal を送れる全 process（session・editor・agent・この harness）に届くので
  system harm として main・subagent とも Deny（Ask ではない）。発火は pid operand が literal `-1` で、
  signal が literal の probe `0` でないときだけ。option 文法は bash の `kill` builtin に従う:
  `-l`/`-L`/`--list`/`--table` は list mode（何も送らない）、`-s SIG`/`-n NUM`/`--signal[=]SIG`/`-sSIG`/`-nNUM`
  は signal 指定、signal 未指定のうちの最初の `-WORD` が signal（`-1` = SIGHUP）で、その後の `-N` は pid、
  `--` 以降と最初の非 option 語以降はすべて pid。したがって `kill -9 -1` は Deny、bare `kill -1`（SIGHUP、pid 無し）・
  `kill -1 1234`・`kill 1234`・`kill -9 1234`・`kill -l`・`kill -0 $$` は従来どおり（Allow）。展開（`$pid`）は
  literal `-1` ではないのでこの rule の対象外（既存の判定のまま）。
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
- **shell へ流し込むブレースグループはメンバー出力の連結を program として読む（`detect/payload/brace_feeder.rs`、
  `detect/ast/support.rs` の `brace_group_stdin`、`detect/ast/b5.rs` の `piped_by_compound_stage`、backlog 7eb86bb3）** —
  `{ echo …; printf …; } | bash` のメンバーが全て素の `echo` か静的に評価できる `printf` で、区切りが
  `;`・改行・`&&` だけのとき、メンバー出力の連結（と各メンバーのリテラル・`printf` の方言ごとの読み）を
  stdin program として判定する。それ以外のメンバー（`cat f` 等）・引用なしの展開・リダイレクト・入れ子の
  pipe/グループ・`||`・`&` を含むグループは Ask のまま。グループがサブシェル `( … )`・`if`・ループ本体の
  中にあるとき、Table B.5 層はパイプ前段が単一の単純コマンドでないリテラル stdin を
  `<shell> -c '<本文>'` として検出器全体で判定する（raise-only）。回帰テスト:
  `crates/blastguard/tests/backlog_20261114_brace_group_feeder_piped_to_bash_fp.rs`、
  `crates/blastguard/tests/brace_group_feeder_adversarial_20261120.rs`。
- **pipe 先の python/node の argv も option 文法で読む（`detect/payload.rs` の pipe 規則、
  `detect/payload/program_source.rs` の `python_program_source`、`detect/payload/node_source.rs` の
  `node_program_source`、backlog f9861c4e、blastguard 0.2.569）** — `python3 -X dev` / `-W ignore`、
  `node -r m` の値を script file と誤認せず、pipe の program を判定する（`File` は script file 実行、
  `-c`/`-m`/`-e`/`-p` は従来の読み、`Undetermined`＝未知の option は pipe/here-document 供給時に Ask を記録）。
  回帰テスト: `crates/blastguard/tests/backlog_20261114_pipe_to_interpreter_with_value_flag.rs`。
- **compound command 内・exec fd・zsh `=()` 経由の shell program も判定する（`detect/payload.rs` の
  `decompounded`、`detect/payload/exec_fds.rs` の `exec_opened_fds`、backlog 72872200、blastguard 0.2.557）** —
  `analyze_interpreter_stdin_exec` は行を原文と compound 構文を外した読みの 2 回判定する（追加のみ。2 つの
  splitter の長さが食い違えば Ask）。`exec N< <(…)` / `exec < <(…)` で開かれた fd から stdin を複製する reader は
  その procsub の program を判定し、どこでも開かれていない fd（`bash <&3`）は、reader が stdin から program を
  読みうるとき Ask。here-string（`exec N<<< …`）と quoted here-document（`exec N<<'EOF'`）で開かれた fd は、
  後の文の reader（`<&N` / `/dev/fd/N`）を中身を inline で渡した形（`bash <<< …`）として判定する
  （`detect/payload/exec_fds.rs` の `cross_statement_fd_verdict`、backlog e686a85e、blastguard 0.2.580。詳細は
  末尾の節「exec が開く fd の文をまたいだ追跡」）。モデル外の形（subshell・関数本体・条件付きの exec、unquoted
  here-document、`N<&M` の連鎖、close 後の読み、subshell・関数本体内の reader、読めない形での fd の言及、本文が値を決めない展開で書かれた fd の読み — `<&$((1+2))`・
  どこでも代入されない `$fd` など）は
  Ask（subagent では Deny）で、中身が literal で harm なら main でも Deny。fd 0 を here-text で開く
  `exec <<< …` / `exec <<'EOF'` も同じ追跡で、stdin を program として読む後の reader について中身を判定し（close の後は対象外。`exec 0<&N` / `exec <&N` で中身が確実に分かる fd N を fd 0 に複製した後は、その中身で判定する。詳細は末尾の節の 5）、literal で harm なら main でも Deny
  （backlog 2210e1db、blastguard 0.2.598、fd 0 への複製は 0.2.600）。それ以外は従来どおり reader が stdin から program を読みうるとき Ask。
  ファイルで開かれた fd は `bash < file` と同じ扱い。
  行の中で literal に束縛された `$fd`（`fd=3; exec 3<<< '…'; bash <&$fd`、`… bash /dev/fd/$fd`）は、どの rule より先に
  `detect/readings/static_bindings.rs` の `resolve_static_bindings` が `3` に置き換えるので、展開で書かれた
  fd としては扱われず literal の `bash <&3` / `bash /dev/fd/3` と同じ判定になる。2 つの slash に挟まれた
  path component としての名前（`/dev/fd/$fd` の `fd`）は束縛の言及に数えない（`detect/vars.rs` の
  `mentions_name_outside_reference`。直後が `=` の形 `x/fd/=2` は算術の複合代入なので従来どおり数える）。
  以前はこれを言及と数えて `/dev/fd/$fd` を解決せず、展開で書かれた fd として Ask にしていた
  （backlog 54231a0a、2026-11-16 のユーザー裁定。
  回帰テスト: `crates/blastguard/tests/backlog_20261116_static_fd_var_matches_literal.rs`、
  `crates/blastguard/tests/backlog_20261114_exec_fd_dynamic_fd_reader.rs` の `statically_resolved_fd_var_gets_literal_verdict`）。
  exec で開かれた stdin は upstream pipe のある reader にも適用する（制限側の過大近似）。zsh の `=(…)` は
  process substitution として読む。回帰テスト:
  `crates/blastguard/tests/backlog_20261114_compound_wrapped_or_cross_stage_shell_program.rs`。
- **pipe で if / while / until の本体や eval / sh -c に渡る program も判定する（`detect/payload/compound_readers.rs`
  の `compound_and_forwarder_pipelines`、backlog b3569da5、blastguard 0.2.569–0.2.571）** — pipe で供給される
  stage が（`!` / `time [-p]` の後ろも含め）`if` / `while` / `until` / `case WORD in` で始まれば、対応する
  `fi` / `done` / `esac` までの各文（条件・全分岐。先頭の `then`/`do`/`else`/`elif`/`{`/`!`/`time`/case の
  pattern 語 `x)` を外す）を `UPSTREAM | member` として `analyze_interpreter_stdin_exec` で再判定する（制限側の
  過大近似）。入れ子の opener はこれらの前置語の後ろでも数える（数え漏れると内側の `fi` が外側を早く閉じ、
  後続の reader が判定されない）。keyword は command 位置（各文の先頭＝`;`・`&&`・`||`・改行の後、各 pipe
  stage の先頭＝`|` の後、およびそれらに続く前置語の後。closer `fi`/`done`/`esac`/`}` の文に続く stage も含む）
  でだけ opener / closer として数え、引数位置（`echo if`）や quote 内の語は数えない（shell にとっても keyword
  ではない）。範囲を判定できない `coproc` や読めない `case` header は Ask。閉じが
  見つからなければ行末までの全文を member にする（Ask にはしない。brace group から分割された member は
  閉じを持たずに届く）。stage の head（exec-wrapper の先も含む）が `eval` ならその引数テキスト（原文と
  外側の quote を 1 層外した読み）、shell の `-c`（`shell_program_source` が `Inline`）なら `dash_c_payloads` の
  各候補の各文を同様に再判定する。回帰テスト:
  `crates/blastguard/tests/backlog_20261114_pipe_to_compound_reader_and_forwarder.rs`、
  `crates/blastguard/tests/backlog_20261114_pipe_to_compound_nested_bang_time.rs`、
  `crates/blastguard/tests/backlog_20261114_pipe_to_compound_keyword_argument_fp.rs`。
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
- **`python3 -c PROG arg…` の `arg` の展開は program ではなく argv（backlog d6de38be）** — Python は
  `-c` で option 列を終え、直後の 1 語だけが program、以降の語は `sys.argv[1:]`。program 語
  （`detect/payload/program_word.rs` の `inline_eval_program_word`）が判定可能で、program 自体に `$`・
  backtick が無く、`SHELL_STRING_EXEC_MARKERS`・動的実行 marker を含まず、Python tokenizer で読んで
  文字列を shell/process に渡さない（`python_strings_never_reach_a_shell`）ときに限り、argv 語の展開で
  「that program contains a shell expansion」の template Ask を出さない
  （`detect/interpreter/invocation.rs` の `python_argv_expansions_are_data`）。program 内の展開は従来どおり
  Ask（ユーザー裁定 2026-11-12）、program 語が判定不能なら従来の判定、未解決の argv path への書き込みは
  path-mutation 側で Ask のまま。RED/対照: `crates/blastguard/tests/backlog_20261118_python_c_argv_expansion_read_as_program_fp.rs`。
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
  `echo`/`printf`/`true`/`pwd`/`ls`/`cat`/`test`（quote・`>` 無し）・`mkdir [-p] DIR...`（`-p` 以外の
  option・`$`・glob 無し、operand 1 個以上。entry を足すだけで既存 target の解決先を変えない）だけで、
  どの segment にも `` ` ``・`(`・`)`・`{`・`}`・`<`・`\`・`&`・改行が無く（例外: write segment 自身の末尾の
  quoted here-doc opener 1 個 `<<'TAG'`/`<<-'TAG'`/`<<"TAG"`、TAG は英数字/`_` のみ。それ以外の `<` は拒否）、
  write segment の後が `|`/`&` でないときも、target が worktree storage root 下の linked worktree checkout 内
  （symlink 無し）なら session の tree とみなす（`detect/rm.rs` の `write_prefix_cannot_relink`、
  `detect/rm/prefix_inert.rs`、backlog 1b23e133）。それ以外は従来どおり拒否。
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
  model しない command（`python3 -c`・`git -C D mv`・本文が決まらない `sh -c "$X"` 等）が relink 語（`ln`/`mv`/`cp`/`rsync`/`install`/
  `ditto`/`symlink`/`rename` 等）と、target の path 成分または非 literal な語を両方含む — は Ask（Allow しない）。
  `/dev/null`・`/dev/stdout`・`/dev/stderr` は対象外。
  segment 自身の command word 以外が走らせる command も同じ model で読む（backlog 00c6c2b8、
  tests/backlog_20261122_relink_symlink_via_subst_failopen.rs、`detect/relink/nested.rs`）: AST
  （`ast::analyze`）が列挙する `$( )`・`` ` ` ``・`<( )` の中の command（write 自身の segment の
  substitution も含む。redirect より先に展開されるため）と、escape の本文 — 本文が literal な
  `bash`/`sh`/`zsh`/`dash`/`ksh -c`、vi 系 editor（`vim`/`vi`/`nvim`/`ex` 等）の `-c`/`--cmd`/`+` の
  `:!cmd`（`silent` 等の修飾は外す）— を、それぞれ独立した command として relink model に掛ける
  （escape 本文の中の escape も深さ 4 まで）。`echo "$(ln -s /etc/x d)"; echo y > d/hosts` は
  `/private/etc/x/hosts` の system directory Deny。segment 自身の word scan がこれらの語だけを見て
  model しない relink 語扱いしていた場合（`` echo `ln …` ``・`bash -c 'ln …'`）はその扱いを外す。
  どの verb を走らせるかは AST の値だけで読む（`l\n` は `ln`）。文面での前段判定は「substitution 開始記号も
  escape を起こしうる語（shell・vi 系・wrapper・展開を含む語）も無い segment は AST を作らない」だけ。
  判定不能 — substitution か escape を起こしうる語を含む segment を AST が列挙できない・escape 本文が
  決まらない（`bash -c "$X"`、command word が展開、wrapper・`eval` 越しの interpreter）・`:!` でも既知の無害な
  ex command（`q`/`wq`/`set …`/行番号/検索 等）でもない ex command・本文に `%`/`#`/`!` を含む `:!`・
  深さ超過 — は model しない relinker として扱う（上の Ask 条件）。
  ただし展開の command word は、行全体の AST 評価（`ast::targets::resolve`）が値を証明していればその値を
  literal として読む（`P=/x/p.sh; …; $P a` は `p.sh` の直接実行で、この行に escape 本文は無い。
  `B=bash && $B -c '…'` は本文を読む）。証明されない展開だけが判定不能。`.`/`source` で行の文面を走らせない
  と読むのは user 裁定 r21（2026-11-18、SUBSET.md）の rustup PATH setup `. "$HOME/.cargo/env"` だけ
  （`.` builtin・prefix 代入無し・operand 1 つ・値が hook の `HOME` の `/.cargo/env` で、行全体の index でも
  同じ値に解決すること。`ast::is_rustup_env_source`）。それ以外の `.`/`source` は従来どおり判定不能
  （path の字句判定は `/DEV/stdin`〈大文字小文字を区別しない FS〉・`/{dev}/stdin`・`CDPATH` で漏れたため採らない）
  （625d16af8 の回帰: `. "$HOME/.cargo/env" && sed -i … f` と loop 内の `$P` 実行が relink の Ask になっていた。
  tests/ast_eval_fp_targets_20261115.rs の sanity_line_172 / fp_line_195）。
  **RESIDUAL**: relink 語も target も文面に現れない
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
    `CODE_BUILDING` marker（`interpreter/code_building.rs`）は case-fold した本文の substring で拾い、
    2 つだけ出現ごとに Python token から免除する: `re.compile(`/`regex.compile(`（module 名が module-level の
    素の `import` だけで束縛され、他では `name.attr` の読み取りにしか使われない時）と、定数 metadata key の
    `globals()[...]` 読み取り（backlog 80b18990 / 2e5e080c）。tokenise できない program と、免除を積極的に
    確定できない出現は marker のまま Ask。substring なので `exec (`・`globals ()[` のように `(` の前に空白が
    ある綴りは marker に掛からない（list の穴であり免除の穴ではない。未修正）。
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
- **`python3 -i script.py` と `python3 -m code`（`-m pdb`）は、script / module があっても stdin を program として読む**
  （`program_source::python_reads_stdin_program`、tests/backlog_20261114_pipe_to_python_interactive.rs）— pipe の program を判定する。
  `-i` 無しの `python3 script.py` は stdin が data のまま（広げない）。
- **`python3 -c` の program が子プロセスを起動するとき、子は program の stdin を継承するので、その子が stdin を
  program として読みうるなら pipe の program を判定する（backlog 3da14bbd、
  `interpreter::python_file::python_program_enters_stdin_console` の `child_process_may_read_stdin` と
  `python_semantics::child_start_lines`、tests/backlog_20261115_python_os_system_nested_repl.rs・
  tests/backlog_20261115_python_c_import_os_stdin_data_fp.rs・tests/backlog_20261116_python_os_system_literal_spelling_bypass.rs）** — `echo 'import os; os.chmod("/etc/hosts", 0o777)' |
  python3 -c 'import os; os.system("python3 -m asyncio")'` と `echo 'chmod 777 /etc/hosts' | python3 -c 'import os; os.system("bash")'`
  は main・subagent とも Deny。**literal な `os.system("…")` / `os.popen("…")` は、その literal が完全に decode できる
  （`Decoded::Value`。raw literal は backslash を含まない）ときだけ静的に解決**する。`\N{…}`・不正な escape・backslash を含む raw literal は
  未確定として制限側（`python_semantics::lex_marking_inexact`）。解決した command line のどれかの語が
  code interpreter・shell・`STDIN_READING_PROGRAMS`（`ipython` 等の閉じた list）、または `$`・backtick、あるいは sh の glob・brace・tilde
  展開の metacharacter（`*`・`?`・`[`・`{`・`~`）を含む語なら stdin を読みうる（`/bin/ba*`・`/bin/b?sh`・`{bash,}` は実行時に shell になりうる。
  quote 内で sh が展開しない場合も保守的に数える）。command line を実行する sh 自身が stdin を code として読む形も
  stdin を読みうる（tests/backlog_20261116_python_os_system_source_stdin.rs）: quote と backslash を外した語が stdin / fd を
  file として名指す（`/dev/stdin`・`/dev/fd/N`・`/proc/…/fd/N`）、`source` か `eval` である（位置を問わず保守的に数える）、
  または command 位置の `.`（前に assignment・option・`(` を含む語・`command`/`builtin`/`exec` 等の閉じた prefix しか無い。
  `ls .`・`find . -name x` の `.` は operand なので数えない）。`os.system(". /dev/stdin")` と `"source /dev/stdin"` は Allow にしない。
  literal の command がどれでもなければ（`os.system("ls")`・`"cat"`・`"grep chmod"`）stdin は data のまま。
  それ以外の子プロセス起動（computed な command `os.system(c)`・argv list・`subprocess.*`・`os.exec*`/`os.spawn*`・`pty.spawn`・
  `asyncio.*`・runner module の値としての参照 `g(os)`・call 結果の属性 `__import__("os").system`・runner module からの
  `from … import *`、alias 解決後）は未確定として制限側（stdin を読む）。**module の import（`import os`）や起動しない関数
  （`os.getcwd()`・`shutil.which`）は子を起動しない**ので stdin は data のまま。
  **LIMIT**: literal command が closed list に無い stdin 読み program を呼ぶ場合は stdin を読まないと扱う。
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
- **quoted `python3 - <<'PY'` program が書いたファイルも、同じ command の別の部分が実行しうるとき同じ判定にかける
  （backlog 245a4c5f、tests/backlog_20261115_python_heredoc_write_then_sh_fail_open.rs、
  `detect/interpreter/python_written_files.rs` と `detect/echo_script_file.rs` の `python_heredoc_files`）** —
  上の `echo`/`printf` の判定（実行しうるかの探索・内容の判定）をそのまま使い、writer を Python token から読む。
  path と内容がともに exact な literal の `open(P,M).write(C)`（`a` は追記、`w`/`x` は置換）と
  `Path(P).write_text(C)`/`write_bytes(C)` だけが内容を保つ。それ以外の書き込み（書き込み可能または mode が
  判定できない `open`/`fdopen`、literal でない path、f-string、`with … as`、copy/move/rename/link 系の呼び出し、
  program のどこかにある動的実行 marker、tokenise できない body）は内容が読めない書き込み（Ask、subagent では
  Deny）、literal でない path は置き場所不明の target として扱う。実行の探索は body を除いた command で行い、
  here-document を開く最初の stage（python reader 自身）は数えない。実行しうる stage が無い書き込みだけの
  program の判定は変えない（書くだけの有害 `.sh` の扱いは裁定待ち）。過剰側: 書き込み可能な `open` を持つ
  program では mode が literal でない `open` や `os.open(P, O_RDONLY)` も書き込みとして数え、動的実行 marker を
  含む program では後続の shell 語があれば Ask になる。**GAP**（判定の外。コード読解による記述）: import alias、
  `tarfile`/`zipfile` の展開、pathlib の `.replace`、`python -c` やファイル program、unquoted delimiter、
  python program の内側からの実行、書き込みと実行の順序。
- **`git apply`/`git am` に here-document で渡した patch の本文を、それが書くファイルの shell として判定する
  （backlog 42741ea3、tests/verify_e70b6a71_commit_heredoc_later_cmds.rs の
  `e_prime_git_apply_or_am_patch_then_running_the_written_file_is_not_allowed`、`detect/git_apply_patch.rs`。
  `detect_bash` の step 0d）** — plain な `git` 語の後に `apply`/`am` を持つ segment の here-document 本文から
  各行の先頭の `+`/空白を 1 つ取った shell view を作り、元の command の判定と shell view の判定の強い方を取る
  （Deny > Ask > Allow）。次の形は読み取りを推測せず Ask: plain な `|`（`||` を含む）の直後の reader、
  here-document の無い `<`、here-document が 2 つ以上か delimiter が読めない、patch header（`diff `・`--- `・
  `+++ `・`@@ `・`Subject:`・`From `）が無い本文。**KNOWN GAP**（判定の外。Allow のままでありうる）: `|&`、
  subshell/group `( )`、前の行末の `|` で続く pipe、`<<-` の TAB 字下げ本文、reader と同じ行にある別コマンドの
  here-document、名前付き patch ファイル、plain でない `git` 語（quote・変数）。
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
- **`model`** — 判定型 `Decision::{Allow, Deny(String), Ask(String)}`（`deny()`/`ask()`/`is_deny()`）。
  system-harm の Deny は `deny_system_harm()` で理由に `SYSTEM_HARM_MARK` を付け、`is_system_harm()` で識別する。
- **`detect/payload/compound_readers`** — `detect/payload` の子 module。pipe で供給される `if`/`while`/`until`
  compound の各文と、`eval`/shell `-c` が実行する command text の各文を `UPSTREAM | member` の合成 pipeline として
  返す（`compound_and_forwarder_pipelines`。keyword は command 位置でだけ数える。`coproc`・読めない `case` header は Ask 理由）。`stdin_exec_verdict` が
  `group_reader_pipelines` の結果に加えて判定する（backlog b3569da5）。
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
- **`log`**（実装は `overwatch::blastguard_log`。2026-11-17 に移設、`blastguard log` は互換の委譲経路、`overwatch blastguard-log` でも同じ）— `blastguard log`: overwatch の violations ledger（`overwatch::store::violations_path`）から
  `source == "blastguard"` の行だけを file 順に列挙する read-only の照会コマンド。stdin を読まず ledger へ書かない。
  フィルタは `--decision deny|ask`・`--rule`・`--since`（RFC3339 または期間）・`--system-harm`・
  `--subagent` / `--interactive`（排他）、`--json` で JSON 出力。旧形式の行に無いフィールドは UNKNOWN
  （text `unknown` / JSON `null`）で、そのフィールドへのフィルタには一致せず除外件数を stderr に出す。
  text 出力は C0 制御文字・DEL・backslash を escape し、C1 制御（U+0080〜U+009F）・U+2028/U+2029・
  bidi 制御（U+061C / U+200E / U+200F / U+202A〜U+202E / U+2066〜U+2069）を `\u{XXXX}` に escape して
  1 entry = 1 行と表示順の不変を保証するが、曖昧さは無くさない（値は quote されない）。厳密な解析は `--json` を使う。
  exit: 0 = 成功（一致なし・ledger 不在を含み、不在は stderr に `no ledger at <path>`）、2 = 使い方の誤り、
  3 = ledger を特定・読めない／不正な行がある（有効な entry は出力したうえで `line <N>`）／stdout 書き込み失敗。
- **relay（enqueue のみ。本体は `askrelay` へ移動 2026-11-17）** — subagent の Ask を main session で裁定させる relay
  （ユーザー裁定 2026-11-14 逐語: 「sub agentのAskはmainで裁定する」「この仕組みはコードで強制する、どうせわすれる」
  「gateにそんな例外は追加してほしくない」「gateもconduktも絶対に勝手に他のworktreeを参照してはならない」）。
  Stop / PostToolUse / PostToolUseFailure の各 hook と `dismiss` は別 GATE プラグイン `askrelay` が持つ
  （`crates/askrelay/README.ja.md`）。queue 形式・操作は `harness_core::relay`（blastguard と askrelay の共有）。
  blastguard に残るのは次の 2 つだけ: (1) `main` の `emit` が subagent の **Bash の `Ask`** を `Deny` に硬化する
  とき（`harden_for_subagent` は blastguard に残る）、`ask_available()` が真（main session が ask できる）なら
  `queued`（id `bgr-<12 hex>`・ts・session_id・agent_id・cwd・command・Ask 理由）を
  `~/.blastguard/relay/<session_id>.jsonl`（`BLASTGUARD_RELAY_DIR` で上書き）へ追記し、id を**表示する** deny 理由の末尾に付ける
  （violation ledger へは付加前の理由を記録するので `rule_id` は動かない）。`Deny`（system harm を含む）・main の `Ask`・
  headless の subagent `Ask` は記録しない。書き込み失敗・panic でも判定は `Deny` のままで、失敗を deny 理由に明記する。
  (2) main session の Bash `Deny` が queue 済み entry と command・cwd バイト一致なら `denied` を記録し、
  deny 理由に `askrelay dismiss <id> --reason "<why>"` を案内する。askrelay が無い・無効でも subagent の `Ask` は
  `Deny` のまま（fail-closed）で、失われるのは main の Stop を止めるリマインドだけ。
  **信頼境界（backlog #1808）**: queue は信頼できる経路ではない — どのローカルプロセスでも任意の session の
  queue ファイルに追記できる。書き手である blastguard は信頼しないまま変更せず、読み手（askrelay の Stop /
  mark-executed / mark-relayed / dismiss）が payload の `transcript_path`（session transcript が信頼の根）で
  行ごとに Verified（行の `session_id` がファイルの session と一致し、`agent_id` の sidechain
  `<session>/subagents/agent-<id>.jsonl` が存在）/ Refuted / Undetermined を判定する。Refuted は Stop を
  block せず、Undetermined は実行手順なしで block する。(2) の `mark_denied` は transcript を受け取らないので
  行の `session_id` 不一致だけを除外する（`denied` は entry を解決しない）。実在する `agent_id` を名乗る偽造行は
  Verified になり、この設計では検出しない。詳細は `crates/harness-core/src/relay.rs` の module doc。
- **`blocklog`**（実装は `harness_core::blocklog`。2026-11-17 に blastguard から移設、blastguard は呼び出しのみ）— block log（ユーザー裁定 2026-11-14 逐語:「…完全なコマンドが残っていないため、
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
- **`detect/ast`**（`mod.rs` / `parse.rs` / `eval.rs` / `eval/words.rs` / `eval/decl.rs` / `eval/cwd.rs` / `arith.rs` / `support.rs` / `targets.rs`）— shell parser（brush-parser `=0.4.0`）＋静的評価層
  （ユーザー裁定 2026-11-15「parser ＋ 静的評価で一体にする」）。`analyze(&str) -> Analysis`
  （`Known(Script)` / `Undetermined(Cause)`、`Cause` = `NotImplemented`/`TooLong`/`TooDeep`/`Timeout`/
  `Parse`/`Panic`/`ZshOnly`/`Unsupported`）と `program_source(&SimpleCommand) -> Option<ProgramSource>`。
  quoted heredoc 本文・quoted word・comment は data、実行される command word と展開だけが code。
  parse 不能・上限超過・zsh 専用構文は `Undetermined` で、呼び出し側は Ask/Deny に倒す（fail-closed）。
  上限はユーザー裁定 2026-11-16: `MAX_PARSE_BYTES` = 8 KiB 超 → `TooLong`、括弧カウンタ
  （`({[` で +1 / `)}]` で −1、quote 無視）深さ `MAX_PARSE_DEPTH` = 16 超 → `TooDeep`。どちらも brush-parser
  より前に判定し、検出器はこれを Deny に解決する。quote-aware な構造 pre-scan（`Scan`）は拒否を増やす方向
  だけの pre-filter として裁定で許容。brush-parser は `catch_unwind` 下で呼び、parse error → `Parse`、panic → `Panic`。
  静的評価（`eval.rs`）は本文から証明できる値だけを `Literal` にし、それ以外は `Undetermined`（推測した literal
  を返さない）: literal 代入・literal for-list・literal cd（相対 redirect 先は cd が無ければ相対のまま、literal cd
  の後は join した絶対パス、不定の cd の後は `Undetermined`）・stdin の literal program・heredoc。
  `=~`/`:~` を含む argv の tilde、`[[ a -eq b ]]` 等の算術代入、subscript 付き name operand は havoc または
  `Undetermined`。subscript 内の command substitution、および本文で決まらない値を読む算術・subscript
  （`arith.rs`。ユーザー裁定 2026-11-16 strict reading）は解析全体を `Undetermined(Unsupported)` にする。
  `declare -i` された name への代入は算術。opaque 状態（`eval`・`source`・`.`・不定の command word 等の後。
  分岐合流では OR）では任意の name が integer 属性を持ちうる（ユーザー裁定 2026-11-16 reading A）ため、
  以後、次の代入が算術として検査される: plain `n=v`・prefix `n=v cmd`・`export`/`declare`/`typeset`/
  `local`/`readonly` の operand（opacity は当該コマンド実行前のもの）・`printf -v` 対象・`read`/`mapfile`/
  `readarray`/`getopts`/`wait` の literal operand 全部と暗黙対象（`REPLY`・`MAPFILE`・`OPTARG`/`OPTIND`）・
  `for` 変数（literal list の各語、不定 list は不定値として）・`${n:=v}` / `${n=v}`（`${a[i]:=v}` も。
  間接 `${!x:=v}` は opaque 中または `declare -i` 名がある間は integer 扱い）。本文で決まらない値や name を
  読む値（`X=bar`・`FOO=/abs/path` を含む）は `Undetermined(Unsupported)`。
  本文が `_` を integer と宣言した後（`declare -i _`・`typeset -i _`・`local -i _`・`-ri _`・
  `declare -i x _`・`-i` の可能性がある不定 option。ユーザー裁定 2026-11-16 A2）は、bash が各 simple command の
  最後の語（引数が無ければ command name。redirect は語でない。`builtin`/`command` 越しも同じ）を `_` に
  代入するため、以後の各 simple command の最後の語を、そのコマンド実行後の状態（成功・失敗の両方）で算術検査
  する（`echo 5` は Known、`echo bar`・`cat` は name を読むので `Undetermined(Unsupported)`）。`_` に integer
  属性を与えた時点で、その時点の `_` の値も算術検査する（bash 3.2.57 で `true 'a[$(c)]'; declare -i _` が `c` を
  実行することを観測。ユーザー裁定 2026-11-16）: 値は直前の simple command の最後の語、直前が無ければ環境からの
  継承値なので `Undetermined(Unsupported)`。`&`・pipeline・compound command・`[[ ]]`・代入のみのコマンド・
  opaque 化の後の `_` は未モデル化として `Undetermined`。**受容済み残余**: opaque 状態だけでは `_` を integer 扱いしない
  （`_` は `integer` 集合だけを見る）。
  `builtin [--] w ...`・`command [-p] [--] w ...`（入れ子も）は unwrap し、包まれた `w` を単独実行と同じ
  効果・代入検査で評価したうえで（declare 系の operand は Field mode のまま）、wrapper 自体は従来どおり
  状態を opaque にする。`command -v`/`-V` は何も実行しない lookup として
  効果なし（opaque 化は残す）。chain 中の不定語・未モデル化 option も opaque 化のみ。
  `support.rs` は状態を持たない補助関数（escape 展開・パス正規化・fd/stdin・pipe stdin）。
  `eval/words.rs` は `eval.rs` の子 module で、parameter 展開の値（`${n:=v}` の integer 検査を含む）と
  展開後の語の最終値（tilde・word split・pathname/brace 展開・heredoc 展開の判定）を担う。
  `eval/decl.rs` も `eval.rs` の子 module で、declare 系 builtin（`export`/`declare`/`typeset`/`local`/
  `readonly`）の integer 属性付与・代入と compound key の算術検査・binding を担う。
  `eval/cwd.rs` も `eval.rs` の子 module で、追跡する cwd（`Cwd`: `Hook` / `Known` / 有限候補集合 `OneOf` /
  `Undetermined`。ユーザー裁定 2026-11-18、下の `detect/ast/judge` の「cwd の到達条件」）と、相対パスをそれに
  対して置く `resolve` を担う（file-growth cap のため `eval.rs` から分割）。**ユーザー裁定 2026-11-19**:
  ① 評価器は hook の cwd（payload の cwd = `SafeRoots::session_cwd`。`readings.rs::detect_bash_every_reading`
  が検出ごとに `eval/cwd.rs::with_hook_dir` で thread-local に設定）を既知の値として持ち、相対の cd
  （`cd .githooks`）も `Cwd::Known` に解決する（payload の cwd が無いときは従来どおり Undetermined）。
  ② `..` を含む相対パス（cd の operand、既知の cd 後の相対 target / redirect）は実パスで解決する
  （`eval/cwd.rs::real_join`）: cwd 自身が canonical であること、最後の `..` までの各要素が存在する
  symlink でないディレクトリであることを確かめ、`..` はその実の親、最後の `..` より後は字句で join する。
  対象が存在しない / symlink を通る（`cd link && rm ../x`）/ canonicalize が失敗する場合は従来どおり
  Undetermined → Ask（subagent では Deny）。
  **path 解決系の検出器が `targets.rs` 経由で読む**（task migrate-path）。`detect_bash_every_reading` は
  static_bindings 解決後の本文を `analyze` し、`Known` のときだけ index を有効にして全 reading を走らせる
  （`Undetermined` なら index なし＝旧経路のまま）。文字列 reading が置けない対象 —— 切り詰め redirect
  （`>`）・追記 redirect（`>>`）の展開入り target と「解決できない cd の後の相対 target」、`rm` の展開入り
  operand、`sed -i`/`perl -i`/`chmod`/`chown`/`chgrp`/`unlink`/`rmdir`/`mv`/`rsync` が書き換える展開入り
  operand（`system_paths.rs`）、`cp`/`mv`/`install` の展開入り destination —— について、`targets::resolve`
  が評価値を返したときだけ、その各値を **既存の述語でリテラル表記と同じく判定する**（system path なら
  system-harm Deny、protected path なら protected の判定、clean なら当該 Deny/Ask は落ちる。`rm` は operand の
  組み合わせごとに `analyze_rm` を通し最も制限的な判定、組み合わせ 64 超は旧経路）。`resolve` が値を返す条件
  （どれか欠ければ `None`＝旧判定のまま）: shell depth 0（入れ子 payload・`find -exec` tail・program text
  は別プログラム）、index が suspend されていない（bash 3.2 reading で modern と異なる位置で閉じた pass
  と comment-hides-quotes 候補は suspend して再判定）、綴りが本文に 1 回以上現れ、その**全出現**が評価済み
  コマンドのちょうど 1 語（argv 語か file redirect の target 語。quoted 文字列・heredoc 本文・代入値・より
  長い語の一部に現れたら不可）、検出器の役割（redirect target / argv）の語が 1 つ以上あり、その役割の全
  評価値（unroll された for の各回を含む）が空でない `Literal` かつ `[A-Za-z0-9/._+,:@%^-]` のみ。相対値は
  cwd に対して `RedirTarget::Path` と同じく解決済み（literal cd の後は絶対、cd が無ければ書かれたまま）。
  他方の役割の出現は値を与えない（redirect rule は argv 語に書き込まない。逆も同じで、各役割はその役割の
  rule が判定する）。`.`/`source`・`command -v` 等で opaque になった後の変数、`cd x && S=v;` の後の `$S`
  （cd 失敗時 unset）、`||` の後、8 KiB 超（`TooLong`）などは評価器が `Undetermined` を返すので旧判定のまま。
  relink（`relink.rs`）・command word・git の whole-tree staging は未移行。stdin/interpreter program
  （`program_source`）の移行は migrate-stdin-program 段で行う。
- **`detect/ast/judge`** — 構造判定の正典（user ruling 2026-11-17）。仕様は同じディレクトリの `SUBSET.md`
  （Table A: 対応する node / expansion の allowlist、表外は `Undetermined`、Table B: 効果 × target の抽出、
  Table C: `PathClass` × 効果 → verdict）。`judge(script, ctx) -> Judgement` と `Effect` / `PathClass` /
  `Finding` / `PathCtx` の型を S1 で凍結した。S1 時点の `judge` と `classify` は stub で、常に
  `Undetermined(Cause::NotImplemented)` / `PathClass::Unresolved` を返し、detect へは未接続（判定に寄与しない）。
  S3（path-cutover）で接続する。
  **S3b（user ruling 2026-11-18）で Table B B.1 の行だけ接続した**: `detect/ast/mod.rs` の `table_b_verdict` を
  `detect/readings.rs::detect_bash_every_reading` が呼び、旧判定へ raise-only（`Deny > Ask > Allow`、同順位は
  旧理由）で畳む。`detect/ast/judge/table_b.rs` の `extract` が 1 つの評価済み `SimpleCommand` について
  `Row::None`（B.1 の行が無い: 未知 program・wrapper・interpreter・不定の command word・`-i` の無い `sed` 等。
  扱いは不変）/ `Row::Undetermined`（行の option 一覧に無い option、option でありうる不定語、`k=v` でない `dd`
  operand）/ `Row::Targets`（行が名指す (効果, target 値) の組。値は書かれたまま、`SimpleCommand::cwd` で解決）
  を返す。option 一覧は SUBSET.md の B.1 行（2026-11-18 裁定で `cp -f/-i/-n/-p/-v`・`sed -E/-r/-n/-s`・
  `chmod/chown/chgrp -v` を追加。一覧外の literal option は単独でも `Undetermined`、`--help`/`--version` は受理）。
  `Row::Undetermined` → Ask（理由に `is outside its SUBSET.md Table B row`、rule id `ast-table-b-undetermined`）、
  値の決まらない target（unquoted glob・`~user`・`$( )`・未束縛 parameter・不定の cd の後の相対パス・stdin から
  読む `xargs` operand）→ `PathClass::Unresolved` finding → Ask（理由に `a target of a SUBSET.md Table B command
  cannot be placed`、rule id `ast-table-b-unresolved`）、`judge` の panic → Ask。subagent では Deny に硬化。
  値が決まった target には finding を出さない（`classify` は S1 stub のまま、置けたパスは旧 string 層が判定）。
  **literal-dir glob（ユーザー裁定 2026-11-19）**: Table B の全行で、target 語が引用外の glob で、最後の `/` までの
  prefix が literal なディレクトリ（`..` component・展開・引用・escape・brace・`~` 無し）、glob 文字が最終
  component だけにあり（`.` 始まりでない）なら、その親ディレクトリの Table C class で置く
  （`table_b.rs::literal_dir_glob`。値は綴りの `Literal`。`rm -rf /tmp/zqxv*` → `/tmp` → Scratch、
  `chown yuki src/*.rs` → `src` → Project。素の glob の親は cwd、相対 prefix は他の相対 target と同じく cwd で解決。
  `cwd_candidates` の候補判定では glob を書かれたまま合成テキストに綴る）。非最終 component の glob（`a/*/b`）・
  `..`（`../*`）・prefix の展開（`$D/*`・`~/*`）・親を置けない glob は従来どおり `Unresolved` → Ask。語の値自体は
  `Undetermined` のままなので、`-` で始まりうる glob（`rm *.o`）には「option でありうる不定語」規則が残る。
  非再帰の `rm` では旧 rm arm も、全 wildcard operand がこの形で親が project（temp/cache 外で `SafeRoots::classify`
  Inside/IsRoot）か `tmp_glob_operand_allowed` の受理する scratch / 自 checkout に置け、親が `.git` component・
  protected path を含まず pattern の一致しうる entry も protected でないとき、named file と同じ arm で判定する
  （`detect/rm.rs::nonrec_glob_placed`。wildcard Deny を置き換える。それ以外は旧判定のまま）。`/tmp` 直下の
  4 文字 head 規則（2026-11-05、`rm -rf /tmp/ab*` は Allow にならない）は維持。
  `analyze` 自体が `Undetermined` のときはこの層は Allow（旧層だけが判定、従来どおり）。
  **cwd の到達条件**: `cd X && … && cmd rel` の `cmd` は cd 成功時しか実行されないので cwd は X に決まる。
  `cd X; cmd rel`・`cd X || true; cmd rel` は cd が失敗した経路（cwd は hook の cwd のまま）も到達し、
  評価器は 2 経路の状態を `join` する（`eval.rs::join`）。**ユーザー裁定 2026-11-18**: 合流する全経路の cwd が
  既知（記号の `Cwd::Hook` か literal の `Cwd::Known`）で、候補が `MAX_CWD_CANDIDATES` = 4 個以下なら、cwd は
  有限集合 `Cwd::OneOf` になり（`SimpleCommand::cwd_candidates`、`None` = hook の cwd）、judge が相対 target を
  候補ごとに判定して最も厳しい結果を採る（全候補が Allow のときだけ Allow）。`classify` が stub の間、候補での判定は
  検出器全体（旧 string 層＋judge）に合成テキスト — hook の cwd なら `<argv>`、既知ディレクトリ d なら
  `cd d && <argv>` — をかけた結果で、既存の述語が literal 表記と同じく operand を置く（`judge::judge_candidates`。
  Ask / Deny は `PathClass::AtCandidate` finding、Deny は `Judgement::Deny`、system-harm の印は保持）。
  例: hook の cwd が `$HOME` で `cd <scratch>; rm -rf .ssh` は hook 側候補 `$HOME/.ssh` で system-harm Deny。
  候補が 4 個を超える（`cwd: more than 4 directories reach here (a cd may have failed)`）、候補が不明（`cd "$X"`・
  `cd -`・不明な hook の cwd からの相対 cd 等。`cwd: differs between the paths that reach here (a cd may have failed)`）、
  argv に合成できない語（非 literal・制御文字・quote が要る command word）がある、hook の cwd が不明、1 script で
  候補判定が 32 回を超える — のいずれかなら `Unresolved` で Ask（subagent では Deny）。judge 以外の読み手
  （`targets.rs`・`RedirTarget::Path`・`ProgramSource::File`）にとって `OneOf` は従来どおり `Undetermined`。
- **`detect/ast/judge/deny_targets`** — Q3/Q4 の deny-target 表（user rulings 2026-11-14、設計承認 2026-11-19、
  backlog 7275b9d9。仕様は `SUBSET.md`「Deny-target table」）。表は 1 つだけ（`TABLE`）で、`ast/mod.rs` の
  `deny_target_verdict` を `detect/readings.rs::detect_bash_every_reading` が `table_b_verdict` の後に呼び、
  raise-only（`Deny > Ask > Allow`）で畳む。照合するのは評価器が解決した path の**値**だけ（B.1 行の target を
  `resolve_at` で解決したもの、cd が無ければ session cwd に結合した綴りも。B.2 の書き込み redirect
  `>`/`>|`/`>>`/`&>`/`&>>`/`<>`/`>&path`）で、生のコマンド文字列・再構成した文字列には一切照合しない。
  S5 の Python `Fs` 効果は shell 綴り（`os.replace` → `mv --`、`os.chmod` → `chmod MODE --`、
  `open(.., 'wb')` → `: >`）経由で同じ表に当たる（第二の表は無い）。行: Q3 = home ディレクトリそのもの
  `/Users/*`・`/home/*`（削除・`mv` の移動元・mode 変更・link のみ）、`**/.ssh`(`/**`)、`**/.gitconfig`
  （$HOME の外でも）。Q4 = `**/.githooks`(`/**`)、`**/scripts/check-*.py`、`**/crates/*/hooks`(`/**`)、
  `**/.claude/plugins/cache/**/{bin,hooks}`(`/**`)。Q3 の home 以外と Q4 は全書き込み効果を Deny し、
  chmod も Deny（従来の chmod arm は read/exec を外す mode だけ）。ただしユーザー裁定 2026-11-20
  「書込み Deny・exec 付与は許可」により、Q4 の 4 行では exec bit を**付与するだけ**の chmod（全語 literal、
  `--` 以外の option 無し、mode が `[ugoa]*+x` の節か octal `755`/`775`）は上げない（従来の判定のまま）。
  それ以外の mode 変更（`777`・`o+w`・`-x`・`000`・`-R …`・`+X`）と Q3 行への chmod は Deny。読み取りは
  照合しない。component 単位・大文字小文字無視の glob（`*` は `/` を跨がない）で、書かれたままの綴りと `..` を
  字句解決した綴りの和集合で照合する（`/tmp/sshx/f`・`/tmp/notscripts/check-x.txt` は一致しない）。safe root・
  scratch・temp・cache の免除は行に効かない。唯一の例外は既存の protected-path lift（user ruling 2026-10-03）で、
  repo 相対の Q4 3 行（`.githooks`・`scripts/check-*.py`・`crates/*/hooks`）は linked worktree checkout の内側
  （`classify_worktree_checkout_value` Inside）では上げない。一致 → Deny（main・subagent とも、理由に
  `is on blastguard's Q3/Q4 deny-target table`、rule id `ast-deny-target`）。表を読み込めず書き込み target が
  ある・B.1 の target 値が不定で他に一致が無い（Table B が同じ値に既に出す Ask と同じ）・panic → Ask
  （subagent では Deny）。それ以外はこの層は意見なし（Allow）。不定の redirect target・未モデルの
  wrapper（`sudo` 等）・`analyze` が `Undetermined`・展開すれば行に当たりうる glob・symlink の着地先・行
  ディレクトリ以外の祖先の再帰削除はこの層では判定しない（旧層の判定のまま）。
- **`detect/heredoc_data`** — `detect_bash` の手順 0a（backlog f6102045 / 19baba2f、user rulings 2026-11-08 /
  2026-11-17 / 2026-11-18）。brush-parser（`detect::ast::analyze`）が DATA と示す QUOTED here-document 本文を、
  string rule が行を読む前に取り除く（`without_data_heredoc_bodies`）。本文が data になるのは次をすべて満たすとき
  だけ（どれか欠ければ全本文を残し従来どおり全 rule で判定）: quoted で本文 span が parse 結果とバイト一致・
  区切り語が素朴・`#!` 行で始まらない／読み手が `cat`（`-` 以外の option 無し）・`tee`（`-a`/`--append` のみ）・
  `git commit -F -` 系で全語が決まり `program_source` が `None`／`cat`/`tee` の行き先が literal の data・非 shell
  コード・JSON/TOML/YAML ファイル（`.md`・`.txt`・`.rs`・`.json`・`.jsonl`・`.toml`・`.yaml`・`.yml` 等。
  `.sh`・拡張子無し・`.py` は除く。TOML/YAML は 2026-11-18 裁定で追加、command を走らせる設定は Protected target
  判定が止める）か端末・`/dev/null`・stderr・`$( )`／同じ行のどのコマンドもその本文を program として走らせえない
  （`.`/`source` は literal `$HOME/.cargo/env` だけ許容）／他コマンドが着地パスを literal 語で名指さない
  （素の `cd`・`heredoc::INERT_LATER_COMMANDS`・素の `cargo test`/`cargo build` は除く。不定の語は本文を除いた
  うえで、全本文を残した行も Allow でない限り Allow を Ask に上げる＝`DataView::floor`）／本文外が printable
  ASCII・空白・tab・改行だけ。
- **`detect/rm/new_layer`** — 新層（`detect::ast`、SUBSET.md Table B/C）による 1 つの `rm` の判定で、その行に
  ついて旧 `rm` string reading を退役させる（user ruling 2026-11-18「ruling 14 replaced per rule」）。
  `known_allow` は、option が Table B の `rm` 行（`-r -R -f -i -d -v` の cluster、`--recursive --force --dir
  --verbose`）だけ・評価器が旧 token を評価済み top-level `rm` に対応付けられ（token は語の綴りそのもの、または
  旧 walk が literal な `cd` の後で書き換えた絶対パス — 評価器が既知の cwd で相対語を解決した値と一致するときだけ。
  `cd /tmp && true && rm -rf lib`・`(cd /tmp && rm -rf lib)` が `rm -rf /tmp/lib` と同じ判定になる。cwd が既知で
  なければ値は絶対にならず対応付かない）operand が 1 つ以上・全 operand 値が
  `Literal`（絶対、または cd 無しで session cwd 相対、`..` 無し、`.` 終端でない）・全 target の Table C class が
  System / Protected 等の先行 class を経たうえで **Scratch** か **OwnWorktree** のときだけ `true`。それ以外は
  「Known でない」で `analyze_rm` は旧判定のまま（この関数は Ask/Deny を返さず、判定を下げない）。
  **rsync の Delete 行（SUBSET.md Table B、user ruling 2026-11-18）**: `rsync --delete`/`--del`/`--delete-*` は
  destination 配下で source に無いものを再帰削除するので、`detect/rm.rs::analyze_rsync_delete` が local
  destination を `rm -rf` の operand として判定する — `new_layer::rsync_dest_allows`（literal 語か評価器が解決
  した展開の全パスが Scratch / OwnWorktree）なら Allow、それ以外は `rm -r -f <dest>` と同じ判定（理由に rsync の
  形を前置）。delete flag 無し・remote destination・list mode はこの rule では Allow（他の rsync rule は残る）。
- **`detect/known_allow`** — `detect/rm/new_layer` の `known_allow` を Table B の全 B.1 行に一般化した新層の
  「Known Allow」（S6-3 で追加、S6-4 で初めて配線）。`known_allow(cmd, rest, ctx)` は、評価器が `cmd` + 旧 token を
  評価済み top-level コマンドに対応付け（`ast/targets.rs::tied_b1_targets`。行に listed な option が別語として
  取った引数〈`truncate -s 0` の `0`〉は operand の綴りとして扱わない — `table_b::option_argument_words`、
  `-t DIR` は target なので除外）、対応付いた全コマンドに target のある B.1 行があり、target が 1 つ以上・全 target 値が
  `Literal`・全 target の Table C class（`judge::classify`）が **Scratch** か **OwnWorktree**、かつどの target も
  lift 無しの述語で command-running config を名指さない（`judge::names_command_running_config`:
  `rm::new_layer::is_protected`・Q3/Q4 deny-target table の全行〈checkout lift 無し〉・任意深さの
  `hooks/hooks.json`。値と physical landing の両方。置けなければ拒否。ただし session 自身の linked worktree
  checkout 内〈`judge::in_own_checkout`: session cwd がその checkout 内、または session cwd がどの linked checkout
  にも無い〈main tree の session。2026-10-03 lift〉。最終成分 glob は親ディレクトリで置く〉の綴りは
  `judge::own_checkout_spelling_names_config` で判定し、lift される 4 entry〈`.githooks/`・`hooks/hooks.json`・
  `crates/*/hooks/`・`scripts/check-*.py`。deny-target 行の `lift_in_checkout`〉を名指さない —
  `.claude/settings*.json` 等の protected 名と lift されない行〈home・`.ssh`・`.gitconfig`・plugin cache〉は残る。
  user ruling 2026-11-20（同日の「Deny のまま」「own worktree 内 config への追記は Ask」をこの 4 entry について
  置き換え。2026-10-03 lift が勝つ）。project main tree・他の worktree〈session が別 checkout にいる〉・置けない
  綴りは全 list のまま）ときだけ `true`。それ以外は「Known でない」で、呼び出し側は旧判定のまま
  （この関数は Ask/Deny を返さない）。呼び出し側: S6-4 — `detect.rs` の旧 `truncate` arm（`true` なら Allow、
  それ以外は `shape_deny_or_confined_ask` の判定のまま。S6 は退役のみで新しい Ask/Deny は無い）、
  S6-5 — `detect.rs` の旧 `dd` arm（`true` なら Allow、それ以外は旧 `of=` 読みの Deny のまま。`dd` の B.1 行は
  `of=F` の `F` が target・`if=` は read・`k=v` 語は丸ごと 1 語なので path 中の `0`/`1` が tie を壊さない。
  `of=DIR/*.txt` の最終成分 glob も literal-dir glob として `DIR` で置く。同一行の literal 代入 `F=…; dd of=$F`
  は評価器が `Literal` に解決し、その値で判定する〈Scratch なら Allow、`/etc/hosts` なら Deny〉。
  tests/s6_5_dd_retire_20261120.rs）、
  S6-6 — 旧 `tee` arm（`detect/tee.rs::analyze_tee`。`true` なら Allow、それ以外は旧判定のまま。`tee` の B.1 行は
  全 operand が target・`-a` で `Append`・それ以外は `Truncate`。option は `-a`/`--append`・`-i`/`--ignore-interrupts`・
  `-p` のみ〈`-ai`・`-ap` の cluster も可〉、それ以外の option は行が `Undetermined` で Known でない。旧 token に残る
  redirect〈`tee F > /dev/null` の `>`・`/dev/null`〉は除いて対応付ける。同一行の literal 代入 `F=…; tee "$F"` は
  旧層が `$F` を値に置換した行が index されるので、代入の値語〈unquoted・値＝綴り〉にある同じ綴りは operand の
  別読みとみなさない〈`targets.rs::Index::tie`。`tee` 行のみ。他の行の同じ形は旧判定のまま —
  `F=<Scratch>; truncate -s 0 $F` は ask/deny、tests/s6_4_truncate_retire_20261120.rs `control_non_literal`〉。tests/s6_6_tee_retire_20261120.rs）。旧 `tee` arm 自体の fail-open 是正（0.2.814）: glob 語は
  「存在しない target」ではない〈最終成分 glob は親ディレクトリの class で置き、Scratch/OwnWorktree かつ
  command-running config でなければ Allow・それ以外は既存 target と同じ Deny・置けない glob は Ask〉。
  `tee -a` と `>>` の command-running config〈`judge::names_command_running_config`、置けなければ該当扱い。
  own checkout 内では `.claude/settings*.json` 等だけ〉への追記は Ask（main）/ Deny（subagent）。own worktree 内の
  `.githooks/*`・`hooks/hooks.json`・`crates/*/hooks/*`・`scripts/check-*.py` への上書き・追記〈`>`・`>>`・`tee`・
  `tee -a`・`truncate`・`dd of=`、literal と最終成分 glob〉は main/subagent とも Allow（user ruling 2026-11-20、
  0.2.821。旧層の protected lift〈`protected.rs::literal_dir_glob_in_worktree_checkout`〉と `classify` の
  Protected lift も最終成分 glob を親ディレクトリで置く）。
  fail-open 是正（0.2.818、backlog 2e704124）: (1) `>` の unquoted 最終成分 glob は 1 つの不在 path として probe
  しない — `tee` と同じ置き方〈`ast::redirect_literal_dir_glob` で `: <word>` の argv 語として
  `table_b::literal_dir_glob` に掛け、`tee.rs::redirect_glob_placement`／`allow_cell`〉で、Scratch/OwnWorktree かつ
  command-running config でなければ従来どおり・それ以外の class は既存 target と同じ扱い〈recovery Undetermined〉・
  置けない glob は Ask。quote／escape された `'*.txt'`・`\*.txt` は literal ファイル名のまま。(2)
  `judge::spelling_names_command_running_config` は最終成分 glob が config 名に一致しうるなら該当扱い
  〈`hooks` 内で `hooks.json` に一致しうる glob、または deny-target 行の直下 entry に一致しうる glob:
  `deny_targets::may_match_child_unlifted`／`globs_may_intersect`。証明できる不一致〈literal 接頭辞・接尾辞〉以外は
  一致扱い。ただし shell glob が `.` で始まらなければ `.` で始まる名前には一致しない — この dot-name 規則は
  判定中の行が dot 一致を有効にしえない〈`judge::dot_match` が `Off`〉ときだけ使う（下の dotglob 是正）。
  quote された `*` を含む literal 名も glob と区別できないので該当扱い〉。(3) `>`（rule 2）も `>>` と同じ
  述語〈`tee.rs::redirect_names_command_running_config`。quote 除去後の名前で判定、除去できなければ該当扱い〉で
  command-running config への上書きを Ask（main）/ Deny（subagent）にする — 上書きは追記より緩くならない
  （どの名前が該当するかは上の own checkout 分割に従う。tests/glob_and_config_write_failopen_20261120.rs）。
  最終成分だけの literal-dir glob（user ruling 2026-11-19）は親ディレクトリで置かれる `Literal` なので
  Scratch 内なら Allow、中間成分の glob は `Unresolved` で旧判定のまま。`rm`・`rsync --delete` は引き続き
  `detect/rm/new_layer`（tests/s6_4_truncate_retire_20261120.rs）。
  dotglob fail-open 是正（0.2.824、tests/dotglob_glob_dotname_failopen_20261120.rs）: 最終成分 glob は
  dot 一致が有効なら `.gitconfig`・`.ssh` 等の dot-name entry に展開しうる。**literal の dot-name 綴りより緩くしない**:
  (a) `detect/ast/judge/dot_match` が最外の行（`readings.rs::every_reading_scoped` で一度だけ）を `Off`／`On`／
  `Maybe` に分類する。`On` = option builtin（`shopt`/`setopt`/`unsetopt`/`set`/`emulate`/`typeset`/`declare`/
  `export`/`local`/`readonly`/`eval`）・子 shell（`bash -O`・`zsh -o`、`-c` 本文も再帰）の語、または代入名が
  （小文字化・`_` 除去・`no` 接頭辞除去・`=` より前で）`dotglob`／`globdots`／`globignore` そのもの、
  `options[…globdots…]`、`set`/`setopt`/`unsetopt`/`zsh` の `-…4…`／`+…4…`（zsh で `set -4`・`setopt -4`・
  `GlOb_DoTs`・`unsetopt noglobdots`・`options[globdots]=on`、bash で `GLOBIGNORE=x` が有効化することを実測。
  `emulate sh/ksh/csh/zsh` は無効のまま）。`Maybe` = 不定の command word、option builtin の不定語、`source`／`.`、
  `options=(…)` 代入、行の解析が `Undetermined` で上記の名前が本文に現れる場合。行の外から読まれたら `Maybe`
  （fail-closed）。`On`・`Maybe` では dot-name 規則を外す。(b) deny-target 表は B.1 の literal-dir glob target を
  「親の直下で最終成分に一致しうる entry」で行ごとに照合する（`deny_targets::glob_rows`。lift は親の直下の
  entry で置く）。dot-name 経由でしか届かない行は `On` で Deny、`Maybe` で Ask、`Off` で上げない。(c) AST が
  word を持たない `>`／`>>` の glob target は旧層 rule 2/2b が `ast::redirect_glob_deny_target` で同じ判定を
  引き、Deny なら Deny。行の外で（rc ファイル・同じ shell の以前の Bash 呼び出しで）有効化された dotglob は
  見えない（hook が判定する shell は無効で起動する前提）。
  heredoc 本文の範囲（0.2.824、tests/quoted_glob_arg_not_redirect_fp_20261120.rs）:
  `heredoc.rs::first_line_quoted_here_document_body` は opener が 1 行目でなくてもよい — 最初の `<<` を含む行より
  前の各行が quote 釣り合い・末尾 `\` 無しなら、その行を opener 行として本文を切り出す（`opener_line`）。
  以前は 2 行目以降の opener では「最初の改行以降すべて」を program 本文とみなし、本文の後ろの shell 行
  〈`mut ratchet 'a > head[k]]'` の quote された引数〉を program text として shell 読みし、0.2.818 の glob
  redirect 判定で Deny にしていた（FP）。opener より前・閉じ行より後の行は他の全規則が通常の shell として判定する。
- **`scope/reading`** — `SafeRoots` の配置述語に渡す operand の読み方 `Reading` と、それに依存する配置の核
  （`classify_disposable_in` / `classify_disposable_value` / `lexical_absolute_as` / `irreplaceable_home_asset_value`
  / `classify_worktree_as` / `classify_against_as`）。`Reading::Spelled` は shell TEXT（展開・再読されうる文字
  ＝`NOT_A_LITERAL_PATH` を含めば `Undetermined`）、`Reading::Value` は評価器が証明した値（quote 除去・展開後の
  `Value::Literal`。`"~/x"` の `~` はディレクトリ名で home ではない。制御文字・空値だけ拒否）。それ以外の規則
  （相対 base・`..` の字句 collapse と残余 `..` の拒否・resolver・最長 root 一致・`IsRoot`・symlink 禁止）は
  Spelled と同一。`scope.rs` から分割（file-growth cap）。
- **`detect/confined`** — confined-ask helper（`shape_deny_or_confined_ask`・`confined_ask`。形だけが理由の
  rule で、全 target が safe root の内側に解決したら Ask、そうでなければその rule の Deny）。`detect.rs` から挙動を
  変えずに移した（file-growth cap）。
- **`detect/payload/echo_output`** — 素の `echo` の出力 helper（`plain_echo_output`: 全語が static で backslash
  を含まない `echo [-neE…] WORD…` の出力、`quote_bare_echo_words`: 同じ出力を pipe rule が literal program と
  読む quoted 形に書き直す）。`detect/payload.rs` から挙動を変えずに移した（file-growth cap）。
- **`detect/ast/s5`** — stage S5。行の中の inline Python program（`python3 -c`・stdin の here-document・
  literal の pipe）を crate `progeffect` で parse し、各 effect を shell の綴りにして full detector で判定する
  （`prepare`）。parser が Known と読んだ program だけを `pass` に置き換えた neutral text を legacy readings に
  渡す。詳細は下の「inline Python program の parser 判定（S5）」。
- **crate `progeffect`**（plugin ではない共有 lib。harness-core と同じく `Cargo.toml` の version だけを持ち、
  `scripts/check-version-bumped.py` の `SHARED_CRATES` に登録）— 埋め込み program の effect を、言語ごとの
  本物の parser で読む。Python は rustpython-parser 0.4（`default-features = false` + `num-bigint`。LGPL の
  malachite を link しない）。effect × target の判定表は持たず、各 effect の shell の綴り
  （`Effect::shell_spelling`）を host に渡す。
- **`lib`** — 上記 module を公開し、検出ロジックを他クレート（specguard forge・condukt scheduler）へ
  再利用可能にする純粋 API（I/O なし）。

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
   - subshell `( … )`、関数本体、条件付きの exec（`&&` / `||` の右辺や `if` の中）、展開を含む here-document（unquoted `<<EOF`）、`N<&M`（M が 3 以上）の連鎖は、追跡の対象外とする。これらの形で開いた fd を読む場合は判定不能として扱う。ただし中身が literal で harm と分かる場合は、裁定 1 により Deny とする。
3. 再現テスト: `crates/blastguard/tests/backlog_20261114_exec_fd_herestring_then_shell_reads_fd.rs`（`exec 3<<< 'chmod 777 /etc/hosts'; bash <&3` は main・subagent とも Deny）。
4. 補足裁定（AskUserQuestion への回答、2026-11-14。specforge rigor 未達の 4 点への回答）:
   - `exec N< file` と `exec N<&0`（`0..2` の標準 fd の複製）を読む reader は、従来どおり `bash < file`（標準 fd の複製なら stdin）と同じ扱いとする（上の「ファイルで開かれた fd は `bash < file` と同じ扱い」を維持する）。
   - brace group `{ … }` の中の exec はモデル化する。既存の `decompounded` の読みと同じく brace を外して、トップレベルの exec と同じに扱う。
   - exec がトップレベルにあり、reader が subshell `( … )` か関数本体の中にある場合は判定不能とする（main で Ask、subagent で Deny）。ただし中身が literal で harm と分かる場合は、裁定 1 により Deny とする。reader が brace の中にある場合は、brace を外した読みと外さない読みの両方で判定し、制限側の verdict を採る（追加のみ）。
   - close（`exec N<&-`）した後で fd N を読む reader は、close 前に開いていた中身が literal で harm なら Deny とする。それ以外は、どこでも開かれていない fd と同じく Ask とする（subagent では Deny）。
   - `<&N` や `/dev/fd/N` で fd を読む reader は、shell（`bash|sh|zsh`）に限らない。既存の stdin reader の集合（code interpreter を含む）のすべてを対象とする。
5. fd 0 への拡張（ユーザー裁定 2026-11-14、backlog 2210e1db。blastguard 0.2.598。fd 0 への複製はユーザー裁定 2026-11-15、blastguard 0.2.600）:
   - 上の追跡は fd 0 にも適用する。`exec <<< literal`・`exec 0<<< literal`・quoted here-document（`exec <<'EOF'`）で fd 0 を開いた後、または here-text で開いた fd N を `exec 0<&N` / `exec <&N` で fd 0 に複製した後、後の文の reader（`bash|sh|zsh`・interpreter・`source` / `.`）を判定する。対象は次の 3 条件をすべて満たす reader だけ: 自分で stdin を redirect しない、pipe からも読まない、program の読み元（reader の option 文法で読んだもの）が stdin か判定不能。`bash script.sh` や `bash -c '…'` は stdin を program として実行しないので対象外。
   - fd 0 の中身は、それより前の exec を書かれた順に反映して決める。判定に使うのは、その時点で fd 0 が確実に持つ literal の中身だけ。close（`exec 0<&-`）やファイルでの開き直し（`exec 0< file`）の後は中身なし、`exec <<< other` で再代入した後は新しい中身になる。`exec 0<&N` / `exec <&N` で複製した後は、その時点で fd N が確実に持つ literal の中身になる（例: `exec 3<<< '…'; exec 0<&3; bash` と `exec 3<<< '…'; exec 0<&3 3<&-; sh` は、fd 0 がその中身を持つ）。中身が判定不能な fd 0 は判定しない。中身の分からない fd（どこでも開かれていない fd、判定不能な fd、`N<&M` の連鎖）からの複製、unquoted here-document、subshell や `&&` の後ろの exec がこれに当たる。
   - 判定は、その reader の pipeline を書かれたまま、その reader に中身を inline で渡した形（`bash <<< …`）で行う。それが Deny（中身が literal で system harm）なら、main・subagent ともに Deny とする（裁定 1）。
   - それ以外（中身が harm でない・判定しない形・上の対象外の reader）では、fd 0 の追跡はこの判定を採らない。fd 0 は解決済みにしないので、reader が stdin から program を読みうるときは従来どおり Ask（subagent では Deny）のまま。たとえば `exec <<< 'echo hi'; bash` と `exec 3<<< 'echo hi'; exec 0<&3; bash` は main で Ask のまま。close の後の reader も同じく従来どおりで、close 前の中身が harm でも、この追跡では Deny にしない（close した fd 0 の中身は reader に届かない）。中身の分からない fd から複製した fd 0 を読む reader は判定しないが、Allow にもしない（main で Ask、subagent で Deny）。中身が確実に分かる fd N から複製した fd 0 は、上のとおりその中身で判定し、literal で system harm なら main・subagent ともに Deny とする（裁定 1）。
   - 再現テスト: `crates/blastguard/tests/backlog_20261114_exec_stdin_herestring_literal_harm.rs`（fd 0 の here-string に literal の system harm を入れて後で `bash` が読む形は main・subagent とも Deny）と、`crates/blastguard/tests/backlog_20261114_exec_stdin_harm_non_stdin_reader_fp.rs`（script ファイルや `-c` を実行する reader は Allow のまま、close の後は main で Deny にしない）と、`crates/blastguard/tests/backlog_20261115_exec_dup_onto_stdin_literal_harm.rs`（fd 3 の here-string を `exec 0<&3`・`exec 0<&3 3<&-`・`exec <&3` で fd 0 に複製して後で shell が読む形は main・subagent とも Deny、中身が harm でない形は直接の形 `exec <<< 'echo hi'; bash` と同じく main で Ask・subagent で Deny、`cat`・`bash script.sh` は Deny にしない）。
   - quote された数字の複製先（backlog cf0bf4ca、2026-11-15）: `<&` / `>&` の複製先の fd 番号は、shell が単語を読むのと同じく quote を外してから数字として読む。`exec 3<<< 'chmod 777 /etc/hosts'; bash <&'3'` と `bash <&"3"` は `<&3` と同じ形として追跡し、main・subagent とも Deny とする。quote を外しても数字にならない複製先（`-` や名前）は従来どおり。quote を外せない複製先（閉じていない quote・ANSI-C `$'…'`・末尾の孤立した backslash）と展開を含む複製先は判定不能として扱い、制限側に倒す（落とさない）。ここでの展開は本文が値を決めないもの（`<&$((1+2))`、どこでも代入されない `$fd` など）で、行の中で literal に束縛された `$fd`（`fd=3; exec 3<<< '…'; bash <&$fd`）は `resolve_static_bindings` がどの rule より先に literal に置き換えるため、literal の `<&3` と同じ判定になる（backlog 54231a0a、2026-11-16 のユーザー裁定。回帰テスト: `crates/blastguard/tests/backlog_20261116_static_fd_var_matches_literal.rs`）。実装は `exec_fds.rs` の `decode_literal_word` で、`exec_fds.rs` にある複製先の単語の読み取りは `exec_fds::dup_fd_number` を、`payload.rs` の `procsub_programs` にある stdin の複製先の読み取りは `exec_fds::stdin_dup_target`（下の移動の形も読む）を通して、すべてこれを使う。読めない形の言及を見落とさないための文字列上の数え上げ（`exec_fds::raw_fd_mentions`）も、`&` の後の単語を同じく quote を外して読み、先頭が fd 番号なら言及として数える。したがって quote した形は、quote しない形と同じ所で同じ数だけ数えられ、判定は quote しない形と同じになる（例: `eval 'bash <&3' 2>&'3'` と `x=$(bash <&3) 2>&'3'` は `2>&3` の形と同じく main・subagent とも Deny、`cat <&'3'` は `cat <&3` と同じ、移動の形 `bash <&'3'-` は `bash <&3-` と同じく Deny）。再現テスト: `crates/blastguard/tests/backlog_20261115_quoted_numeric_dup_target.rs` と、quote した形と quote しない形の判定の一致を見る `crates/blastguard/tests/backlog_20261115_quoted_numeric_dup_consistency.rs`。
   - stdin への移動の形（backlog 967fd65c、2026-11-16）: reader の stage の `<&N-` / `0<&N-`（fd N を stdin へ移動し N を閉じる）は、bash が展開・quote 除去の後に末尾の `-` を 1 つ外して読むのと同じく、`<&N` と同じ stdin の複製として読む（`exec_fds::stdin_dup_target`）。したがって `bash <&3-` と `bash 0<&3-` は `bash <&3` と同じく main で Allow にせず、subagent で Deny とする。`-` だけ（`<&-`、stdin を閉じる）は何も読まない。`-` を外しても fd 番号にならない移動の形（`<&abc-`）は stdin の出どころが判定不能として制限側に倒す。`exec` の行の複製（`exec 4<&3-`）は `exec_fds::dup_fd_number` のままで、この読み替えの対象外。再現テスト: `crates/blastguard/tests/backlog_20261114_stdin_dup_move_fd_suffix.rs`。

## inline Python program の parser 判定（S5）— ユーザー裁定 2026-11-17

裁定（`crates/blastguard/src/detect/ast/SUBSET.md` の「Python programs (S5)」に逐語で引用）:

- 構造の方向（2026-11-17）: 個別の patch は当てない。埋め込まれた Python は本物の parser で読み、その effect を
  shell と同じ effect × target の規則（Table B / C）で判定する。対象は `python3 -c`、`python3 - <<'EOF'`、
  stdin から渡る Python。
- parser は rustpython-parser 0.4（`default-features = false` + `num-bigint`）。malachite（LGPL）は link しない。
- 言語ごとの parser と effect × target の規則は 1 つの共有 lib crate（`crates/progeffect`）に置き、blastguard と
  将来の script-quarantine plugin が使う。
- text と code の区別は parser が決める（heuristic にしない）。文字列 literal・comment・f-string の literal 部分は
  data で、effect になるのは呼び出しだけ。
- 判定不能（動的 dispatch: `getattr`・`__dict__`・`exec`/`eval`/`compile`・`__import__`・`importlib`・
  `globals`/`vars`/`locals`、追えない alias、literal でない argv、parse error）は main で Ask、subagent で Deny。
- system harm はどの wrapper を通しても Deny。
- `python3 x.py` は S4 の B.5 Undetermined のまま。S5 の対象は inline の program だけ。
- legacy の文字列読みを外すのは parser が Known を返した program だけ。Undetermined の program では legacy の
  読みを raise-only で残す（S3b と同じ型）。

振る舞い（`detect/ast/s5.rs::prepare`、`detect/readings.rs` の `every_reading_scoped` から呼ぶ）:

1. 行の各 simple command について、`env`/`nohup`/`timeout`/`nice`/`time`/`exec`/`command` を外した先が
   `python*` で、program が `-c` の literal か literal の stdin（here-document・here-string・literal producer の
   pipe）なら、`sys.argv`（`-c` は `['-c', …]`、stdin は `['-', …]` か `['']`）と一緒に `progeffect::python::analyze`
   に渡す。`-i`・`-x` は判定不能。
2. Known なら、各 effect を shell の綴りにして full detector（`detect_bash_every_reading`）で判定する。
   `os.system(S)` などの shell 実行は `S` そのもの、ファイル操作は `rm -- 'P'`・`: > 'P'`・`mv -- 'A' 'B'` など
   （一覧は SUBSET.md）。command の cwd が literal なら `cd 'D' && ` を前に付け、cwd が候補集合か判定不能なら
   effect を持つ program は判定不能。reason は ``the Python program calls `CALL` on `PATH` (judged as `…`): `` に
   続けて shell 側の reason（rule id は shell 側の id。既存ファイルの truncate は
   `interpreter-unrecoverable-write`）。
3. Known の program は、neutral text（その program を `pass` に置き換えた行）を legacy readings に渡すことで
   legacy の文字列読みから外す。外さない（legacy を元の text で残す）のは: Undetermined、command / process
   substitution の中・他の command の word / redirect / here-document 本文の中・pipeline の最後でない stage、
   effect（または出力 redirect）があって行の他の command がファイルを実行しうる（write-then-run は legacy と
   harness-5b の担当する読みに残す）、`-c` の program が command を実行し行が Python の stdin を与えている、
   neutral text が同じ command 列に再解析できない、のいずれか。
4. 判定不能・未読の一覧、既知の gap（here-string / pipe の program は外さない、下の形以外の `bash -c` の中の
   Python は legacy だけ、`sudo` 等の wrapper、同じ行で effect model の無い program〈`git checkout`・`tar`・
   `curl -o` 等〉が作る shadowing ファイル）は SUBSET.md に置く。
5. S5b（ユーザー裁定 2026-11-19、詳細は SUBSET.md「S5b follow-ups」）:
   (a) program が import する module `M` について、実行ディレクトリに `M.py`・`M/`・compiled な `M` があれば
   判定不能（Ask）。ディレクトリを list できない（IO エラー、payload に cwd が無い、cwd が判定不能）ときも判定不能。
   (c) 素の `bash -c SCRIPT` / `sh -c SCRIPT`（`b5` の wrapper の後。`sudo` は対象外）の literal script は、直接書いた
   のと同じく S5 で判定し、Known の program は外側の行の legacy readings から外す。
   (d) shell の `-c` payload の候補は `-c` 以降の word 境界だけ（word の途中から始まる断片は候補にしない）。
   `sh -c 'echo "$HOME"'` は Allow。
6. S5c（ユーザー裁定 2026-11-20、詳細は SUBSET.md「S5b follow-ups」(a')）: 同じ行で program より前に走る command
   （program 自身の出力 redirect を含む。program が top-level でない・行に loop / function があるときは行の全 command）が、
   program の実行ディレクトリに import する module を shadow する entry（(a) と同じ綴り・大文字小文字無視、その下の
   path も含む）を書きうるなら判定不能（Ask）。読む write は既存の target 抽出だけ: 出力 redirect（B.2）、B.1 の作成系
   target（`cp`/`mv`/`install` の destination、`ln`、`tee`、`truncate`、`dd of=`、`sed -i`、`find -fprint`）、および
   この読み専用の `mkdir`/`touch` 行（`judge/table_b.rs::entry_writes`。S3b の judge は不変）。write target を
   置けない・row 外の option・unwrap できない wrapper・実行ディレクトリそのものへの write（`cp x .`）・real path を
   読めない、も判定不能。`s5.rs::same_line_shadow`。受け入れテスト: `tests/s5c_sameline_shadow_20261120.rs`。

受け入れテスト: `tests/backlog_20261119_python_heredoc_string_literal_system_harm_fp.rs`・
`tests/backlog_20261118_python_heredoc_string_literal_rm_home_fp.rs`・`tests/backlog_20261118_python_c_string_literal_rm_fp.rs`・
`tests/backlog_20261118_python_c_argv_system_write_not_deny.rs`・`tests/backlog_20261116_python_dict_lookup_system_group_pipe.rs`・
`tests/ast_effect_rules_20261117.rs`（c1873b73 の形）・`tests/backlog_20261116_python_os_system_literal_spelling_bypass.rs`。

## 決定済みの producer pipeline の出力を program として読む（backlog 40dc52c0）

実装: `detect/payload/determined_output.rs`（契約の全文は module doc）。呼び出し側は
`detect/payload.rs`（procsub / stdin の program 読み）、`detect/readings.rs`（`detect_bash_every_reading` の
chain 値）、`detect/ast/b5.rs`。

- 目的: `bash <(echo hi | cat)`・`bash <(seq 10 | sort)`・`bash <(echo "$(echo hi)")`・`echo hi | cat | bash`
  のように、shell に渡る program がコマンド行だけで決まる形を、Ask にせず program として判定する。従来の読みは
  `echo` / `printf` 1 段の literal しか読まなかった。
- 計算するのは、brush-parser が単一の pipeline（simple command だけ。list・`&&`/`||`・`&`・`!`・`time`・
  subshell・group・関数・redirect・代入・process substitution を含まない）と読む text で、先頭が emitter、
  後続がすべて filter の形だけ:
  - `echo`: operand がすべて決定済みの word で、`-` で始まらず backslash を含まない。
  - `seq LAST` / `seq FIRST LAST` / `seq FIRST INCR LAST`: 符号なし 10 進 literal（先頭 0 なし、9 桁以下、
    `INCR` > 0）、出力は `MAX_SEQ_LINES`（1000）行以下。
  - `cat`: operand も option も無い（stdin をそのまま流す）。
  - `sort`: operand も option も無く、入力の行順がどの locale でも同じ（全行が数字だけ、または全行が同じ）。
    それ以外は collation に依存するので決定済みではない。
  - 決定済みの word: plain な unquoted text、single-quoted text、または text と `$( )`（その program 自体が
    この形の pipeline、`MAX_DEPTH` = 3 段まで）だけから成る double-quoted 列。unquoted の `$( )`・backquote・
    parameter・escape・tilde・`$'…'`・`$"…"` は決定済みではない。
- それ以外の形は計算しない（`None`）。呼び出し側は従来の読みと Ask を保つ。計算結果を使うのは
  `beyond_plain_echo`（filter・`seq`・command substitution を使った）のときだけで、従来の読みが扱う形の判定は
  変えない。
- 前提条件 `resolution_fixed`: `echo`/`seq`/`cat`/`sort` が別物に差し替わりえないこと。AST 層で `Known`、関数定義
  なし、どの command にも代入（`PATH=…` を含む）が無く、command word が modelled な名前か program を読む
  shell / interpreter のどれか、を満たすときだけ true。それ以外の command（`alias`・`hash`・`enable`・`export`・
  `cd` など、無害なものも含む）があれば false（許可リスト方式。未知の差し替え手段を通さない）。
  利用者の行・途中の派生 text すべて・判定対象の text の全部に要求し、chain 値は狭める方向にしか変わらない
  （`readings::chain_names_fixed`、`Ctx::names_fixed_narrowed_by`）。false なら何も計算しない。
- 判定不能は制限側: parse 不能・panic（`catch_unwind`）・上限超過はすべて `None` で、従来の Ask に戻る。
- `echo -e`（先頭 operand が単独の `-e`）の escape は `\xHH`（印字可能 ASCII か空白）と `\n`（改行、
  backlog 11b0b4da）だけを復号する。`bash <(echo -e 'echo hi\nrm -rf /etc')` は 2 行の program として
  行ごとに判定され、有害な行が Deny になる。それ以外の backslash・制御文字は決定済みではない。
- `-c` の operand と `eval` の operand（backlog 28019c35 / d766b5fe / c602d32e / bf40851f、
  `detect/ast/b5/determined.rs`）: shell の `-c W` と `eval W…` で、`$( )` を 1 個以上含み値が決まる word
  （`substituted_word_value`。各置換の末尾改行は除く）の値を full detector で program として判定する
  （`judge_body`）。採用するのは Deny のときだけで、Ask / Allow なら従来の Undetermined（Ask）を保つ
  （raise-only。何も緩めない）。前提条件は上と同じ chain 値と `resolution_fixed`。
  - `-c` の flag 配置（`dash_c_operand`）: `shell_source` がモデル化する文字の cluster のどこかに `c`
    （`-c`・`-ec`・`-xc`）、`-c` の前の option（`-x -c`）、cluster 末尾の `-o` / `-O` が次の word を取る形、
    `--` / `-` による option 終端（`-c -- W`）を読む。long option・`+` option・`-s`・未モデルの文字・`-c` 無しは
    計算しない（従来の Ask）。値が `-` で始まるときも計算しない。
  - `eval`: 先頭の `--` を 1 個捨て、全 operand の値（literal はそのまま、`$( )` を含む word は計算値）を
    空白 1 個で連結して判定する。値の決まらない operand が 1 個でもあれば計算しない。`command [-p] [--] eval`
    と `builtin [--] eval` も同じ（`runs_eval` / `determined_builtin_eval`）。`command -v` / `-V`・未知の option は
    `eval` ではない。判定対象の text 自身にだけ、その 1 個の `eval` を許す
    （`resolution_fixed_but_one_eval` / `readings::chain_names_fixed_but_one_eval`。外側の chain は厳格なまま）。
    2 個目の `eval` があれば計算しない。
  （RED は `tests/backlog_20261121_determined_subst_shape_gaps.rs`。）

## stdin / process substitution の program 読みで `echo` / `printf` が shell 関数でありうるときは Ask（backlog 1e9da081 / f06ecb05）

実装: `detect/payload/function_shadow.rs`（契約の全文は module doc）。呼び出し側は
`detect/payload.rs`（`analyze_interpreter_stdin_exec` の stdin program 読み。`shadowed_emitter_ask`）、
`detect/readings.rs`（`detect_bash_every_reading` の chain 値 `CHAIN_FUNCTIONS` / `chain_functions`）、
`detect.rs`（`Ctx.functions` の初期値と、`detect_bash` が判定する text ごとの `Ctx::functions_widened_by`）。

- 問題: `echo 'x' | bash` や `bash <(echo x)` の読みは、上流 stage の quoted literal を program 全体とみなす。
  これは `echo` / `printf` が自分の引数しか出力しない（`echo_output::stage_emits_only_its_arguments`）ことに
  依存する。同じ名前の shell 関数が定義されていると、その名前は関数本体を実行し、出力は引数ではない。
  `printf(){ echo 'chmod 777 /etc/hosts'; }; bash <(printf hi)` と
  `echo(){ echo 'chmod 777 /etc/hosts'; }; export -f echo; bash -c 'bash <(echo hi)'` は、判定した program が
  `hi` になり Allow だった。
- 関数名の読み取り（`defined_functions`）:
  - text が関数定義の形を含まない（`function` という文字列も、空白類〈space・tab・newline・CR〉だけを挟んだ
    `(` … `)` も無い）ときは定義なし（`Known` の空集合）。bash / zsh の定義構文は `NAME ( )` と `function`
    キーワードだけである。
  - 定義の形を含み、かつ zsh の `functions`（`dis_functions` を含む）parameter を名指ししうる出現 —
    `functions` の直後が英数字・`_`・`/`・`.`・`-` 以外（`[`・`=`・`+`・`:`・`}`・空白・quote・演算子・
    text 末尾）— があれば `Undetermined`。`functions[echo]='…'` などの代入は関数を定義するが読み取らないため。
    bare word の `functions` も `Undetermined`（制限側）。
  - それ以外は AST 層の `analyze_with_function_definitions` で読み、`Known` なら `Script::function_names`、
    `Undetermined`（名前が literal でない、評価できない構文を含む等）なら `Undetermined`。
  - `Undetermined` はどの名前も関数でありうる（`may_shadow` が true）と扱い、和集合では吸収する。
- chain 値: 利用者の行と、判定対象に至る途中の派生 text すべての定義の和集合。`detect_bash_every_reading` が
  再入ごとに広げて戻り時に復元し、`detect_bash` も判定する text 自身の定義で広げる。広がる方向にしか
  変わらない。定義の位置（reader の前か後か）や `export -f` の有無は問わない（過大近似。
  `echo(){ :; }; bash -c 'echo hi | bash'` も Ask）。
- 判定（`shadowed_emitter_ask`）: 上流 stage が `stage_emits_only_its_arguments` を満たすとき、その stage の
  command head すべて（emitter と前置の exec-wrapper。`env` という関数も対象）のうち chain 値で関数でありうる
  名前があれば Ask。brace group の `echo` / `printf` member から program を読んだ場合（group feeder）は
  `echo` と `printf` を調べる。Ask 理由は「`{name}` in the stage piping into it may be a shell function defined on
  this command line (or on a line enclosing it)」。literal の判定の後に記録するので、literal 内の Deny が優先する。
- alias（backlog cffe54c4）: alias も同じ類で、`alias echo='echo chmod 777 /etc/hosts #'; bash <(echo hi)` は
  alias の出力が program になる。`defined_functions` は関数名に加えて、parse した script の simple command のうち
  command name が（quote 除去後に）`alias`（`builtin` / `command` の前置は構造的に剥がす: 入れ子の `builtin`/`command`・`-p`・`--` は飛ばし、`-v`/`-V` は何も
実行しない、それ以外の option や literal でない word は `Undetermined`。`ali\as`・`a""lias` も該当）の
  ものの `NAME=VALUE` operand の NAME を chain 値へ足す（`defined_aliases`。text 検索ではなく AST を読む。
  `expand_aliases` の有無は問わない制限側の過大近似）。`alias` が引数（`grep alias f`・`echo alias $HOME`）や
  quote 内にあるだけなら何も足さない。command name が literal でない（`alias` を綴りうる。`a=al; ${a}ias echo=x`
  もここで `Undetermined`）、operand が literal でない、AST が読めない場合は `Undetermined`。ただし `a` `l` `i` `a` `s`
  がこの順に現れない text は `alias` を綴れないので parse せず定義なし。`alias ll='ls -l'` のように
  emitter 名でない alias は判定を変えない。
  テスト: `crates/blastguard/tests/verify_cffe54c4_alias_emitter_controls.rs`。
- 対象外（定義が見えないので何も足さない）: source されたファイルや startup file で定義される関数、hook 自身の
  環境から継承される関数、どちらの形も literal に綴らない text の `eval`（`eval $'echo\x28\x29 …'`）。
- テスト: `crates/blastguard/tests/verify_1e9da081_function_shadow_controls_20261121.rs`、
  `crates/blastguard/tests/backlog_20261121_functions_substring_feeder_fp.rs`。

## shell / interpreter へ pipe する前段の exec wrapper（`env echo` / `command printf`）は 1 本の pipeline のときだけ外して読む（backlog 879c5424）

- `env` / `command` は PATH や関数が与えるものを実行するので、前段 `env echo "ls" | sh` の literal が program だと
  言えるのは、その行の書かれ方がそれを決めるときだけである。
- 外して読む条件（`detect/readings.rs` の `bare_wrappers_as_written`）: 最上位の行（`-c` / stdin 本体ではない）であり、
  かつ行全体が、先頭 stage が bare な `env` / `command` である `|` だけの pipeline 1 本
  （`ast::b5::bare_wrapper_pipeline`）であること。他の文・複合コマンド・prefix 代入があれば外さない。
- 外さないとき、`detect/payload/upstream_stage.rs` の `unresolved_wrapper_ask` が **Ask**（subagent では hardened Deny）を
  足す。raise-only であり、system harm の Deny 等、他の規則の判定を下げない。前段に wrapper が無い・
  brace group の feeder（別規則）・`-c` / stdin 本体では何も足さない。
- PATH 書き換え・関数上書きの手段を個別に列挙して塞ぐ方式は採らない（構造で決める）。
- テスト: `crates/blastguard/tests/exec_wrapper_producer_controls.rs`、
  `crates/blastguard/tests/file_content_piped_to_shell_twins.rs`。

## 登録済み `/flow` driver セッションでは `ask` を出さない — ユーザー裁定 2026-11-21（backlog 4ef3a9e2）

裁定（逐語）: 「非破壊はAllow、他はDeny」、同日の補足「またaskされてたけど、これもallowに」。
/flow は自律ループであり、hook の `ask` は Bypass mode でも dialog として出て loop を止めるため、
driver セッションでは `ask` を出さない機構を blastguard 自身に置く（LLM の記憶に依存しない）。

- driver 判定（`src/flow_driver.rs`）: payload の `session_id` と `cwd` から、backlog の driver registry
  `~/.backlog/drivers/<fnv1a64(canonical main tree) %016x>/<fnv1a64(session_id) %016x>.driver` を読み、
  `now - heartbeat_at <= 1800` のときだけ `Known(true)`。layout と project 正規化は
  `crates/backlog/src/driver.rs`・`store::canonical_project_id` の read 側の写し。ファイル無し・stale・
  別セッションは `Known(false)`、読めない・壊れている・session id が空は `Undetermined`。
  `Known(true)` 以外は driver として扱わない（判定不能を Allow にしない）。
- Table B.5 層（`scope.rs::driver_trusts_program`、`b5.rs`）: driver セッションでは、session scratchpad
  （`/private/tmp/claude-<uid>/<slug>/<session id>/scratchpad/`、`claude-<uid>` は uid 所有の実ディレクトリ）
  内、または自 worktree の `target/` 配下の通常ファイルをトップレベルで直接実行するものに意見を出さない。
  同じ行にそのファイルへ書き得るものがあるとき、system bin だけの `PATH` 以外の注入変数があるときは除く。
- 出力（`main.rs::emit`）: driver セッションで残る `Ask` は元の理由付きで `deny` として出し、block log に
  `ask-hardened-to-deny` として payload ごと記録する。2 度目 deny の ask 化は行わない。`Deny` は不変。
- 登録が無い・stale・別セッション・registry 読み取り不能・payload パース不能では判定を変えない。
- RED: `crates/blastguard/tests/backlog_20261121_flow_driver_session_never_asks.rs`。
