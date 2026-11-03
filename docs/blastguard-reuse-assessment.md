# blastguard の部品は他機能で使えるか — 再利用性の評価

> 対象: `detect.rs` を 1 モジュール 1 コミットで分割した直後の blastguard
> (`crates/blastguard/src/detect.rs` 2402 行 + `src/detect/*.rs` 21 ファイル)。
> 元の依頼: 「他の機能で使用できそうかという観点でも考えてほしい」。
> 本書は**評価と推奨だけ**であり、コードは一切変更していない。「移した」「切り出した」
> 「テストした」と書いた箇所はなく、すべて「こうすべき/すべきでない」という提案である。

## 0. 測定条件と注意

- 測定 rev: `fcbcfc3f`、測定日: 2026-11-03。以降の数値はすべてこの rev・日付の値で、
  使った コマンドを併記する。数値は継承せず、使う側で測り直すこと。
- 各コマンドは `crates/` 直下（`/Users/yuki/.condukt/worktrees/run-20261103-162350-31430-t9/crates`）
  で実行した。`wc -l` は `crates/blastguard` 基準。
- 行数（`cd crates/blastguard && wc -l src/detect.rs src/detect/*.rs`）:
  `detect.rs` 2402 / `segment.rs` 441 / `heredoc.rs` 852 / `substitution.rs` 332 /
  `words.rs` 491 / `vars.rs` 615 / `protected.rs` 486 / `tests.rs` 5021 / 合計 18892。
  `scope.rs` 2480 / `exclude.rs` 1147 / `approve.rs` 641 / `reversible.rs` 1012
  （`wc -l src/scope.rs src/exclude.rs src/approve.rs src/reversible.rs`）。
- バージョン: blastguard 0.2.245、harness-core 0.2.87（`grep -n "^version" crates/blastguard/Cargo.toml crates/harness-core/Cargo.toml`）。

### 0.1 位置の訂正

依頼文は `quote_aware_words` を `words.rs` の関数として挙げたが、実体は
**`crates/blastguard/src/detect/vars.rs:141`**（`pub(super) fn quote_aware_words(seg: &str) -> Vec<String>`）
にある（`grep -n "fn quote_aware_words" -r crates/blastguard/src` の結果）。
`first_shell_word` は `words.rs:20`（`pub(super) fn first_shell_word(s: &str) -> Option<String>`）。

### 0.2 blastguard の誤検知により書き換えた検索

blastguard の PreToolUse hook が、検索コマンドのパターン中に書いた「rm の再帰削除オプション付き
リテラル」を引数のデータではなく rm コマンドとして読み、ブロックした
（hook の出力: `PreToolUse:Bash hook error: recursive rm (-r) can delete an entire directory tree`）。
この誤検知は別途起票される（本タスクでは起票しない）。呼び出し元の許可（読み取り専用の検索に限る）
に基づき、次のとおり書き換えた。測定の再現にはこの対応を見ること。

| 元の意図 | 実際に実行したコマンド |
|---|---|
| blastguard 外で `command` 文字列をリテラル照合している箇所（`git commit`/`git push`/`git add`/rm の再帰削除/`cargo test`）を探す | `grep -rnE "contains\(\"(git commit\|git push\|git add\|cargo test)" '--include=*.rs' . \| grep -v "^./blastguard/\|/tests/"` — **rm の再帰削除の選択肢を落とした**。したがってこの検索は「rm 側の部分一致」を調べていない（未確認）。 |

別件: 最初の試行では `--include=*.rs` を引用符なしで書いたため、zsh が
`no matches found` で失敗した（誤検知とは無関係）。以降は `'--include=*.rs'` と引用して実行した。

## 1. 評価軸（CLAUDE.md §3）

共有クレートへ移す部品の最重要基準は「判定不能を、空集合・`false`・既定値に潰さないこと」。
`harness_core::verdict::Determination<T>` は `Known(T)` / `Undetermined(Undet)` の二値
（`crates/harness-core/src/verdict.rs:458`）。`Verdict` は `Clean`/`Violation`/`Undetermined` の三値。
CLAUDE.md が挙げる三値 `Absent / Undetermined / Known(T)` のうち、`Absent` は
`Determination` にはない（`sed -n 455,466p crates/harness-core/src/verdict.rs` で確認）。

## 2. 候補ごとの評価

### 2.1 シェル字句解析グループ（segment / heredoc / substitution / words / vars）

1. **位置**（`grep -n "^pub(super)" crates/blastguard/src/detect/{segment,heredoc,substitution,words}.rs` の結果から主要なもの）:
   - `detect/segment.rs:63` `split_segments`、`:141` `split_segments_paren_aware`、`:269` `split_segments_with_separators`
   - `detect/heredoc.rs:114` `here_document_openers`、`:244` `here_document_layout`、`:402` `strip_data_here_document_bodies`
   - `detect/substitution.rs:95` `scan_balanced`、`:255` `executed_substitution_payloads`、`:328` `command_substitution_payloads`
   - `detect/words.rs:20` `first_shell_word`
   - `detect/vars.rs:141` `quote_aware_words`（`words.rs` ではない。0.1 を参照）
2. **類似コード**（blastguard 外）:
   - `grep -rnE "fn (split_(segments|commands|command_line|shell|on_operators|compound)[a-z_]*|shell_words|shell_split|tokenize[a-z_]*|split_pipeline[a-z_]*|split_chain[a-z_]*)\b" '--include=*.rs' . | grep -v "^./blastguard/"`
     → シェル分割の同等品は **ヒットなし**。`tokenize` のヒット（`playbook/src/retrieve.rs:44`、`stuckguard/src/sig.rs:71`、
     `specguard/src/forge/gather.rs:383`、`fugu-router/src/rag.rs:16`、`context-governor/src/defaults/injector.rs:30`、
     `compass/src/freshness.rs:251`、`harness-core/src/lessons.rs:238`、`harness-core/src/code_index.rs:398`、
     `harness-core/src/text_index.rs:145`）は自然言語の語分割でシェルの字句解析ではない。
   - `grep -rnE "shlex" '--include=*.rs' .` → `condukt/src/verify.rs:1663`
     （`shlex::split(s).unwrap_or_else(|| s.split_whitespace().map(String::from).collect())`）。
     `condukt/Cargo.toml:29` に `shlex = "1"`。ヒアドキュメントを含む語は
     `grep -rliE "heredoc|here_doc|here-doc" '--include=*.rs' .` で `sitecraft/src/gate.rs` などに現れるが、
     `sitecraft/src/gate.rs` が heredoc を解析しているのか言及だけなのかは**未確認**（本文を読んでいない）。
3. **想定する利用者**: 現時点で呼び出す側として根拠を示せる crate は**ない**。
   - Bash の `command` 文字列を実際に調べているのは `fde/src/gate.rs:147`（`tool_input.command` の取り出しのみ。
     字句解析は呼んでいない）、`stuckguard/src/sig.rs:105`（`"Bash" => norm(field(&inp, "command").unwrap_or(""))`、
     `norm` は `split_whitespace().join(" ")`、`sig.rs:58`）、`ctxrot/src/hooks/preguard.rs:241`（`command` の取得と抜粋のログ）。
     いずれも空白区切りで足りており、引用符・ヒアドキュメントの解釈を必要としていない（3 箇所のコードを読んだ範囲での観察）。
   - `condukt/src/verify.rs:1663` の `parse_argv` は done_criteria 内の「`cargo test -p x`」のような単一コマンドの argv
     化であり、`;`/`&&`/ヒアドキュメント/コマンド置換を扱わない。字句解析グループを使う動機は薄い。
4. **blastguard 型との結合**: 型としては小さい。`grep -c` で数えた `Ctx`/`Decision`/`SafeRoots` の出現数は
   segment/heredoc/substitution/words/vars のどれも `Ctx=0 SafeRoots=0`、`Decision` は vars.rs に 1 件のみ
   （`for f in segment heredoc substitution words vars protected; do grep -c ... detect/$f.rs; done` の結果）。
   ただし **関数の相互依存が結合の本体**:
   - `segment.rs:1` `use super::{here_document_layout, opens_unmodelled_construct, scan_balanced, Stop};`
   - `heredoc.rs:1-4` `use super::{basename, git_subcommand_index, is_code_interpreter, is_shell, quote_aware_words, split_segments_with_separators, strip_version_suffix, SeparatedSegment};`
   - `vars.rs:1-5` が `here_document_layout`/`split_segments` を逆向きに参照
   - `words.rs:1` が `is_code_interpreter`（`interpreter.rs:26`）、`xargs_command_start`（`find_xargs.rs:304`）を参照。
   → segment ⇄ heredoc ⇄ vars が循環し、さらに `interpreter`/`find_xargs`/`git` といった判定側の語彙に伸びる。
   切り出すにはこの循環を断つ設計変更が要る。
5. **判定不能の扱い**: 三値でも `Determination` でもなく、`Option` / 空 `Vec` / 暗黙の既定値に潰れている。
   - `scan_balanced`（`substitution.rs:95`）は `Option<usize>`。doc は「`None` = 閉じていない」であり、
     呼び出し側の約束事で「不在として扱う」。引用:
     「An unterminated quote or substitution ... yields `None` — the same safe direction every other extractor in this file takes: an unbalanced construct is one a real shell would not run as written either, so nothing further is asserted about it.」
     → 深さ超過（`MAX_QUOTE_PAREN_DEPTH = 64`、`substitution.rs:34`）も同じ `None`。「閉じていない」と「解析不能」を区別できない。
   - `quote_aware_words`（`vars.rs:141`）は `Vec<String>` を返し、閉じていない引用符は黙って最後の語に含めて返す
     （末尾 `if !cur.is_empty() { out.push(cur); }`）。フラグはない。
   - `first_shell_word`（`words.rs:20`）は `Option<String>`。doc の「Deliberately NOT a full shell lexer: no expansion of `$VAR` ...」のとおり、
     展開は未対応で `None` と「語なし」の区別もない。
   - `split_segments*`、`here_document_openers` は `Vec` で、空は「何もなし」と「読めない」の両方を意味しうる。
     heredoc.rs の doc は「an opener whose delimiter cannot be read, and a delimiter line that never arrives, both leave the body open to the end of the line」
     と、**呼び出し側が後段で restrictive に倒す前提**を散文で述べている。
   これらは blastguard の内側では「後段の `Decision::Ask` が受ける」ことで成立している。
   共有クレートへ移すと、その後段が消える。CLAUDE.md §3 の「エラー時に空の集合を返さない」に直接該当する。
6. **判定: defer（推奨しない）**。
   - 理由 1: 呼び出し側が現存しない（上の 3.）。需要が未観測の抽象化を作ると、§6 の「そういうことにしておく」を増やす。
   - 理由 2: 戻り値が `Option`/空 `Vec` で、§3 の基準を満たさない。移すなら先に各関数を `Determination` を返す形に
     書き換え、`None`/空の意味ごとにテストを書く必要がある。これは移動ではなく新規設計で、規模は大きい。
   - 理由 3: 循環した依存（4.）のため、グループの一部だけを切り出せない。
   - 再検討の起点: blastguard 以外の crate が Bash 文字列を「引用符・`;`・ヒアドキュメント込み」で解釈する要件を持った時点。
     その時は `condukt` の `shlex` 利用（`verify.rs:1663`）が、未終了引用符を `split_whitespace` に落とす fail-soft であることも
     §3 の観点で併せて見直すこと（本書は未検証）。

### 2.2 `physical_landing`（symlink 解決と hop 上限）

1. **位置**: `crates/blastguard/src/detect/protected.rs:144`
   `fn physical_landing(target: &str, base: Option<&str>) -> Option<Determination<String>>`。上限は `protected.rs:129`
   `const MAX_SYMLINK_HOPS: usize = 40;`。呼び出し側は `protected.rs:78`、`protected.rs:295`。
2. **類似コード**:
   - `grep -rnE "read_link|symlink_metadata" '--include=*.rs' . | grep -v "^./blastguard/"` →
     `read_link` は **blastguard 外でヒットなし**。`symlink_metadata` は存在確認の用途で
     `specguard/src/report.rs:162`、`specguard/src/gatecheck.rs:403`、`backlog/src/store.rs:2857`、`backlog/src/claim_ledger.rs:516`、
     `donegate/src/git.rs:142`、`donegate/src/gate.rs:476`、`sitecraft/src/config.rs:218`、`sitecraft/src/decide/record.rs:540` など。
   - 近い意図の実装: `crates/fde/src/gate.rs:171` `pub fn canonical_lenient(p: &Path) -> Determination<PathBuf>`
     （最長の既存 prefix を `std::fs::canonicalize` し、残りを `Component::ParentDir => base.pop()` で字句的に処理する）。
     `harness-core/src/discovery.rs:111` `lexical_normalize` と `harness-core/src/projkey.rs:287` は字句的なだけで symlink を解決しない。
     `canonical_lenient` の呼び出し元は `fde/src/gate.rs:233,361,365` のみ（`grep -rn "canonical_lenient" '--include=*.rs' .`）。
3. **想定する利用者**: `fde`（`fde/src/gate.rs:361` `Request::Edit { tool, path } => match canonical_lenient(&path)`）が
   書き込み先の実体を解決する用途で同種の問題を持つ。`donegate`/`tdd` など他の gate が「書き込み先が守る領域に着地するか」を
   判定する場合も呼び手になりうるが、そのような呼び出しは現在の diff では確認していない（未確認）。
   `fde` が `physical_landing` に置き換えるべきか、`..` の扱い（`canonical_lenient` は存在しない尾部の `..` を字句的に `pop`、
   `physical_landing` は解決済み prefix に物理的に適用して missing_depth を管理）の差が結果に出る入力があるかは**未検証**
   （両者を同じ入力で走らせるテストを書いていない。評価者が書くこと）。
4. **blastguard 型との結合**: **ほぼない**。引数は `&str` と `Option<&str>` のみで、`Ctx`/`Decision`/`SafeRoots` を使わない。
   `protected.rs:1` の `use super::{exclude, has_glob_meta, Ctx, Decision, Determination};` のうち、本関数が使うのは `Determination` のみ。
   切り出しで切るのは、(a) `$`/`` ` `` を含む文字列を `None` にする入口の前処理（blastguard の構文都合）と、
   (b) 内部の `use std::path::{...}` のみ。
5. **判定不能の扱い**: **三値（Option + Determination）で、概ね良好**。引用:
   - 「`None` = not a path this can place (an unexpanded `$`/backtick, or relative with no absolute base). `Undetermined` = the filesystem could not answer: a symlink loop, a `read_link`/`lstat` error other than "does not exist".」
   - hop 超過: `if hops > MAX_SYMLINK_HOPS { return Some(Determination::undetermined(...)) }`
   - `read_link` 失敗: `Err(e) => return Some(Determination::undetermined(format!("symlink `{}` could not be read ({e})", ...)))`
   - `NotFound` 以外の `symlink_metadata` エラー: `Err(e) => return Some(Determination::undetermined(...))`
   - `NotFound` だけは「未作成の尾部」として `missing_depth` に積む。
   懸念は **`None` の層**。`Determination` に `Absent` がなく、共有版にするなら `None` を `Undetermined`（理由付き）に
   統一するか、呼び出し側に前処理を移すのが望ましい。今の `Option<Determination<_>>` のままだと、`None` を
   `?`/`unwrap_or` で潰す呼び出しが生まれうる（blastguard 内の呼び出し `protected.rs:78` は `physical_landing(...)?` で伝播している）。
6. **判定: recommend（条件付き）**。
   - 理由: 結合がなく、三値で、`fde` に同種の関数が既にあるため需要が実在する。symlink を物理解決する関数は
     blastguard 外に存在しない（`read_link` が 0 件）。
   - 条件: (1) 戻り値を `Determination<String>` に正規化する（`None` を理由付き `Undetermined` へ）。
     (2) 移す前に、`canonical_lenient` との差が出る入力（symlink を含む尾部、`..` で既存部へ戻る経路、循環）を
     利害関係のない Agent がテストにする。差が出なければ統合、出れば fde の挙動を blastguard 側へ寄せるかを判断する。
     (3) 移した時は blastguard 側を共有版の呼び出しにし、同一コミットで doc を揃える。

### 2.3 `VerdictAcc` と解析予算

1. **位置**: `crates/blastguard/src/detect.rs:829` `struct VerdictAcc`、`impl` は `:836`。
   `decision_to_verdict`/`verdict_to_decision` は同ファイル内（`VerdictAcc` 直後）。
   予算: `detect.rs:775` `const MAX_ANALYSIS_NODES: u32 = 2_000;`、`:779` `static ANALYSIS_BUDGET`（thread_local の `Cell<u32>`）、
   `:795` `fn take_budget()`、枯渇時の応答は `:1017`（`Decision::ask("command is too complex to analyse within the safety budget — ...")`）。
2. **類似コード（harness-core 側）**: `crates/harness-core/src/verdict.rs:311` は既に `detect::VerdictAcc` を名指ししている。引用:
   > 「This is the shared shape behind every hand-rolled "combine sub-analysis results, worst wins" accumulator this repo keeps reinventing (e.g. blastguard's `detect::VerdictAcc`, which ranks its own three answers `Deny > Ask > Allow` — the identical ordering, just named differently).」
   実体は `Verdict::worst_of`（`verdict.rs:323`）。`VerdictAcc` の doc も同じことを述べる
   （`detect.rs` の `VerdictAcc` 直前: 「Internally this ranking is exactly `harness_core::verdict::Verdict`'s own `Violation > Undetermined > Clean` priority (`Verdict::worst_of`)」）。
   予算の類似物: `grep -rnE "thread_local!|MAX_.*(NODES|BUDGET)" '--include=*.rs' crates | grep -v "^crates/blastguard/"` は**実行していない**ので、
   「予算機構が他にあるか」は**未確認**。
3. **想定する利用者**: `VerdictAcc` については、**共有する中身はすでに harness-core に存在する**（`Verdict::worst_of`）。
   他 crate が必要なら `Verdict::worst_of` を直接呼べば足りる。`VerdictAcc` が持つ独自の振る舞いは
   (a) Violation を見たら即 return する短絡、(b) 最初の Undetermined だけを残す（理由を最も外側・最も早い構文に固定する）
   という 2 点のみ。
4. **blastguard 型との結合**: `Decision`（`model.rs:5`）との往復変換が本体。`record(&mut self, d: Decision) -> Option<Decision>`。
   予算は `thread_local!` と `detect_bash` 入口（`detect.rs:201` `ANALYSIS_BUDGET.with(|b| b.set(MAX_ANALYSIS_NODES))`）に
   結び付いている。
5. **判定不能の扱い**: `VerdictAcc` は三値（`Verdict`）で良好。予算枯渇も `Decision::ask`（=`Undetermined` 相当）に倒す
   （`detect.rs:1007-1019` のコメント: 「Budget exhaustion is the definition of "unknown": we did not finish looking, so we have no verdict about the command. That is an ASK」）。
   `finish` が `Verdict::worst_of(self.rest)` に空の `Vec` を渡すと `Clean` になるが（`verdict.rs:311` 周辺の doc:
   「An empty iterator yields `Clean`」）、`VerdictAcc` では `rest` が空＝一度も Undetermined を見なかった場合で、短絡した Violation は
   `record` が先に返すので整合している（コードを読んだ範囲）。
6. **判定: reject（`VerdictAcc` の共有化）/ defer（予算）**。
   - `VerdictAcc`: 共通部分は既に共有版にある。残りの短絡と first-only を共有 API にすると、`worst_of` の「全 reason を連結」と
     意味が分岐する。重複を増やすだけなので reject。
   - 予算: thread_local のグローバル状態を共有クレートに置くと、利用者間で干渉しうる。他 crate に同種の要件があるか未確認（上記 2.）
     のため defer。要件が見つかったら、状態を引数で渡す形（`Budget` 値型）に設計し直すこと。

### 2.4 `scope.rs` の `normalize_abs` / `lexical_absolute`

1. **位置**: `crates/blastguard/src/scope.rs:1624` `fn normalize_abs(path: &str) -> Option<String>`、
   `scope.rs:896` `pub(crate) fn lexical_absolute(&self, operand: &str, cwd: Option<&str>) -> Determination<String>`。
2. **類似コード**:
   - `grep -rnE "fn (normalize_abs|lexical_absolute|lexical_normalize|normalize_path|canonical_lenient|resolve_parents|collapse|normalize_command)\b" '--include=*.rs' .` →
     `overwatch/src/changeset.rs:46 normalize_path`、`harness-core/src/discovery.rs:111 lexical_normalize`（Path ベース）、
     `fde/src/gate.rs:171 canonical_lenient`、`blastguard/src/exclude.rs:319 collapse` / `:364 resolve_parents`、
     `blastguard/src/approve.rs:169 normalize_command`、`scope.rs:896`/`:1624`。
   - `..` を字句的に解決する別実装: `harness-core/src/projkey.rs:287`、`fde/src/gate.rs:184`（`grep -rnE "Component::ParentDir" '--include=*.rs' .` の結果）。
   - harness-core の `session_roots`（1005 行）/ `worktree_origin`（327 行）は**別の課題**（「セッションが書いた git checkout の集合」と
     「消えた linked worktree の元リポジトリ」の永続記録）。`pub fn resolve(...) -> Determination<SessionRoots>`（`session_roots.rs:323`）、
     `pub fn lookup(...)`（`worktree_origin.rs:202`）。パスの字句正規化は提供していない（`grep -n "pub fn" crates/harness-core/src/{session_roots,worktree_origin}.rs` の結果）。
     したがって `normalize_abs` を session_roots/worktree_origin に統合するのは**用途が違う**。比べるべき相手は
     `discovery::lexical_normalize` と `projkey.rs:287`。
3. **想定する利用者**: `normalize_abs` と同種の字句正規化を必要とするのは `discovery.rs`（Path ベースで、`PathBuf` を返す）、
   `projkey.rs`、`fde`、`overwatch`。ただしこれらは既に自前の実装を持ち、置換の動機（バグ・不一致）は**観測していない**。
4. **結合**: `normalize_abs` 自体は `&str -> Option<String>` の純関数で結合なし。`lexical_absolute` は
   `NOT_A_LITERAL_PATH`（scope.rs の const）と `&self`（`SafeRoots`）に依存する。
5. **判定不能の扱い**: `normalize_abs` は `Option<String>`（非絶対パスのみ `None`）で、**絶対パスは常に `Some`**。
   `..` は「Popping past the root simply stays at the root」と POSIX 準拠で root に留まる。判定不能の概念が元々ない純字句関数。
   `lexical_absolute` は `Determination<String>` を返し、空・非リテラル・相対で cwd なし・cwd が非絶対 などをすべて
   `Determination::undetermined(...)` にしている（`scope.rs:896` 以降のコードで確認）。良好。
6. **判定: defer**。
   - `normalize_abs` は 20 行程度の純関数で、共有の利益が小さい。
   - 同種実装が 4〜5 箇所あるのは事実（2. の grep）だが、**差が実害になっているか**は未検証。バックスラッシュの扱いなど
     意図的な差もある（`scope.rs:1618` のコメント: 「Backslashes are NOT translated to `/` (unlike `crate::exclude::normalize`)... there, a wrong guess over-matches into the protected set, which is the safe direction; here it would over-match into the SAFE set」）。
     字句正規化を 1 本化すると、この方向性の違いが消える危険がある。
   - 統合するなら、まず 5 実装を同一入力で比較する特性テスト（差の一覧化）を別 Agent に書かせてから判断する。

### 2.5 `reversible::GitTreeProbe` と `harness_core::git_probe`

1. **位置**: `crates/blastguard/src/reversible.rs:364`
   `pub type GitTreeProbe = fn(&str, &[String]) -> Determination<TreeGitFacts>;`、実体 `probe_tree` は `reversible.rs:473`、
   `TreeGitFacts` は `reversible.rs:333`。`scope.rs:577`（`git_tree_probe: Option<crate::reversible::GitTreeProbe>`）、`scope.rs:722 with_git_tree_probe`。
2. **類似コード**: `crates/harness-core/src/git_probe.rs`（185 行）。`RepoProbe`（`Repo`/`NotRepo`/`Undetermined`、`:25`）、
   `decide_repo_probe`（`:59`）、`dot_git_present`（`:117`）、`probe_repo`（`:141`）、`probe_repo_scrubbed`（`:162`）。
   **blastguard は既に使っている**: `reversible.rs:45` `use harness_core::git_probe::{probe_repo_scrubbed, RepoProbe};`
   （`grep -rn "harness_core::git_probe\|git_probe::" '--include=*.rs' . | grep -v "/tests/"` の結果。他に `donegate/src/git.rs:10`、`reviewgate/src/git.rs:36`）。
   `git_env::scrub` も共有されている（`reversible.rs` の `probe_tree` 周辺の doc に `harness_core::git_env::scrub`）。
3. **想定する利用者**: `GitTreeProbe` は「ある絶対パス以下のバイトがすべて HEAD に載っているか」（`status`/`ls-files`/`check-ignore` の
   結果）を答える。`git_probe` が答える「リポジトリか」とは**問いが違う**。同種の「作業ツリーの変更が git で復元可能か」を
   必要とする crate は `donegate`/`reviewgate`/`tdd`（変更集合を見る gate）が候補だが、現状は `donegate/src/git.rs`、`reviewgate/src/git.rs`
   とも `RepoProbe` を使い、`TreeGitFacts` 相当のものは**確認していない**（それらの diff 取得部は読んでいない。未確認）。
4. **結合**: `GitTreeProbe` は `Determination<TreeGitFacts>` と `fn` 型エイリアスだけで、型としては軽い。ただし
   `probe_tree_with_timeout` 内に blastguard 固有の知識（build 出力ディレクトリ候補 `build_dirs`、`TREE_GIT_TIMEOUT = 1500ms`、
   exit code の一覧）が入る。`scope.rs:722` の `SafeRoots` への注入口は blastguard 固有で、切り出し対象外。
5. **判定不能の扱い**: **良好**。引用（`reversible.rs` の `probe_tree` doc）:
   「`Undetermined` — which the caller reads as "not Allow" — when the path does not exist (`lstat` fails), when it is not inside a git work tree, and when any git invocation fails, times out, exits with a code not listed here, or prints something unparseable.」
   `git_probe.rs` の冒頭 doc は、同じ失敗モード（「"git could not be run" became "this is not a git repository"」）の是正であることを述べる。
6. **判定: defer**。
   - `RepoProbe` 部分は既に共有化済みで追加の作業はない（事実）。
   - `probe_tree`/`TreeGitFacts` は blastguard の「削除が復元可能か」専用で、他 crate の需要を観測していない。
     需要が出た時は、`TreeGitFacts` から build_dirs 周りを切り離し、`Determination<TreeGitFacts>` を維持して移す。
   - 時間制限つきの git 実行（`probe_tree_with_timeout`）は、`harness_core` 側に同等のものがあるかを**未確認**（`grep` していない）。

### 2.6 `exclude::normalize` と `approve::normalize_command`

1. **位置**: `crates/blastguard/src/exclude.rs:434` `pub fn normalize(raw: &str) -> String`
   （= `resolve_parents(&collapse(raw))`、`collapse` は `:319`、`resolve_parents` は `:364`）。
   `crates/blastguard/src/approve.rs:169` `pub fn normalize_command(command: &str) -> String`
   （`command.split_whitespace().collect::<Vec<_>>().join(" ")`）。
2. **類似コード**:
   - `approve::normalize_command` と**同じ式**: `stuckguard/src/sig.rs:58` `fn norm(s: &str) -> String { s.split_whitespace().collect::<Vec<_>>().join(" ") }`
     （`sed -n 58,60p crates/stuckguard/src/sig.rs`）。
   - `exclude::normalize` の近縁: `overwatch/src/changeset.rs:46 normalize_path`（先頭の `./` 除去・連続 `/` の圧縮・小文字化。`..` は解決しない）。
     doc に「Rules (identical to `schedule::normalize_entry`)」とあり、`schedule::normalize_entry` は `condukt/src/schedule.rs:73` にある（`grep -n "normalize_entry" crates/condukt/src/schedule.rs`。本体と overwatch 版の一致は未確認）
     
   - `exclude::normalize` は `\` を `/` に置換し（Windows 風の綴りで保護パスを逃さないため）、`..` を解決し、
     前後の引用符を 1 層外す。`overwatch` の版は大文字小文字を畳む。**意図が違う**（保護集合への過剰一致 vs 衝突検出）。
3. **想定する利用者**: `normalize_command` は `stuckguard`（同式、`sig.rs:58`）が現に同じことをしている。
   ただし stuckguard はシグネチャの安定化が目的、blastguard は承認指紋（`approve.rs` の `fingerprint`）のキーが目的。
   両者が同じ関数を共有すると、片方が「`  ` を残す」変更をしたときにもう片方の指紋が変わる（結合の増加）。
   `exclude::normalize` の共有候補は `overwatch`/`condukt` だが、上記のとおり意図が違う。
4. **結合**: どちらも純関数で `Ctx`/`Decision`/`SafeRoots` に依存しない。`exclude::normalize` は `exclude.rs` 内の `collapse`/`resolve_parents`/`has_unresolved_parent` を伴う。
5. **判定不能の扱い**: どちらも `String` を返し、**判定不能を表現できない**。
   - `normalize_command` は空文字を返しうる。`fingerprint` が `if normalized.is_empty() { return Determination::undetermined("empty command text"); }`
     （`approve.rs`）で受けており、**呼び出し側で潰している**。共有関数が空を返すと、その呼び出し側規律が失われる。
   - `exclude::normalize` は doc が「Still deliberately NOT resolved here: `~` expansion, symlinks, and environment/glob expansion」と述べ、
     解決できない綴りも黙って文字列として返す。判定不能は `is_protected_path` 側の「両方の綴りに一致」で restrictive に倒している。
6. **判定: reject**（共有化）。
   - `normalize_command` は 1 行の式で、共有の利益より「意図が違うものを結合する害」が大きい。stuckguard の `norm` と同一なのは事実だが、
     重複の実害は観測していない。
   - `exclude::normalize` は blastguard の保護パス判定と一体で、方向性（過剰一致が安全側）が固有。他 crate が使うと、安全方向が逆になる箇所で誤用される。

### 2.7 その他の候補

#### 2.7.1 `first_shell_word`（1 語抽出）

`words.rs:20`。`Option<String>`。blastguard 外の呼び出し候補は確認できていない（2.1 の 3.）。2.1 に含めて **defer**。

#### 2.7.2 `protected` / `exclude` の保護パス表

`exclude.rs` の `ALLOW_GLOBS`/保護集合は gate の設定ファイル一覧を持つ。他 gate が同じ一覧を必要とする可能性はあるが、
どの crate が呼ぶかを示す根拠コードを見つけていない（`grep -rn "exclude::" '--include=*.rs' . | grep -v "^./blastguard/"` は
`blastguard/tests/` 配下のみヒット）。**判定: defer**（需要未観測）。

#### 2.7.3 `fde` の `canonical_lenient` との関係（再掲）

2.2 の条件 (2) と同じ。評価の取りこぼしを避けるため、`fde` を優先して照合する。

## 3. まとめ

| 候補 | 位置 | 判定 | 要点 |
|---|---|---|---|
| シェル字句解析グループ | `detect/{segment,heredoc,substitution,words,vars}.rs` | **defer** | 利用者が現存せず、戻り値は `Option`/空 `Vec` で §3 を満たさず、モジュール間が循環 |
| `physical_landing` | `detect/protected.rs:144` | **recommend（条件付き）** | `Ctx` 結合なし・三値。`Option` の層を `Determination` に正規化し、`fde::canonical_lenient` との差を先にテストで確定する |
| `VerdictAcc` | `detect.rs:829` | **reject**（共有化） | 共有部分は `Verdict::worst_of` として既存（`verdict.rs:311`） |
| 解析予算 | `detect.rs:775,779,795` | **defer** | thread_local のグローバル状態で、他 crate の需要が未確認 |
| `normalize_abs` / `lexical_absolute` | `scope.rs:1624,896` | **defer** | 同種実装が複数あるが、差が実害かは未検証。`session_roots`/`worktree_origin` とは用途が別 |
| `GitTreeProbe` | `reversible.rs:364` | **defer** | `git_probe` は共有済み。`probe_tree` は blastguard 固有の問い |
| `exclude::normalize` | `exclude.rs:434` | **reject** | 保護集合への過剰一致という安全方向が固有。意図の違う実装を結合する害 |
| `approve::normalize_command` | `approve.rs:169` | **reject** | 1 行の式。stuckguard の `norm` と同式だが、共有すると指紋が結合する |

## 4. 未確認の項目と理由

- `sitecraft/src/gate.rs` が heredoc を解析しているか: 本文を読んでいない。
- `fde::canonical_lenient` と `physical_landing` の挙動差: 同一入力で走らせるテストを書いていない（本書は分析のみ）。
- 他 crate の予算機構・時間制限つき git 実行・時間制限つき git 実行: 該当 `grep` を実行していない。
- 0.2 の「rm 側の部分一致」: 2. の検索パターンから外したため調べていない（0.2）。
- 上の全項目で、実装・移設・テストは**行っていない**。実施する場合は CLAUDE.md §2 に従い、利害関係のない Agent がテストを書き、
  RED を観測してから進めること。
