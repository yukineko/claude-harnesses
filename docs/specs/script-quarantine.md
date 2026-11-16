# script-quarantine（スクリプト検疫）仕様 — draft

> **状態: draft（2026-11-17）。実装なし。** この文書は設計仕様であり、どの crate の挙動も変えない。
> 「未解決論点」（§9）に挙げたものは人間の裁定が出るまで決定ではない。推奨案を添えているが、推奨は決定ではない。

本文の各記述には次のラベルを付ける。

- **[決定]** — ユーザー裁定 2026-11-17 に由来する。この仕様の中では変更しない。
- **[観測]** — 測定コマンド・版・日付つきで観測した事実。
- **[提案]** — この仕様が置く案。実装前のレビューで覆してよい。
- **[論点 Qn]** — 人間の裁定が要る。§9 の番号を指す。
- **未確認** — 確かめられていない。推測として扱わない。

参照する一次資料（実在を確認済み、rev `6024bd3a3`）:

- `crates/blastguard/src/detect/ast/SUBSET.md`（以下 SUBSET）— Table A / B / C、「Undetermined handling」節、「Stage map」節
- `crates/blastguard/src/detect/ast/judge.rs` — `Effect` / `PathClass` / `Finding` / `Judgement` / `PathCtx` / `judge` / `classify`（S1 stub）
- `crates/blastguard/src/detect/ast/mod.rs` — `ProgramSource`（`InlineLiteral` / `File(Value)` / `Stdin` / `Undetermined`）、`prog::program_source`
- `crates/blastguard/src/approve.rs` — TOFU 承認記憶（`fingerprint` / `Lookup` / `Store` / `TargetProbe`）
- `crates/blastguard/src/interactive.rs` — `ask_available`（Ask を出せないときに Deny へ硬化させる）
- memory: `script-quarantine-plugin.md` / `blastguard-ast-structural-direction.md` / `blastguard-scope-only-command-safety.md`

---

## 0. 前提となるユーザー裁定（2026-11-17）

1. **[決定]** blastguard が判定するのはコマンドの文字列だけである。本文がコマンドの中に無いファイルを実行する形は、blastguard では Ask にする（SUBSET ruling 10、Table B.5 の script-file 行）。
2. **[決定]** 別 plugin（script quarantine）が、実行の前にファイルを検疫する。言語ごとの parser でファイル内容を解析して安全かどうかを判定し、安全なら内容 hash で記録する。同じ hash は次回から自動で通す。
3. **[決定]** 言語ごとの parser と「効果 × 対象パス」の判定規則は、共有 lib crate 1 つにまとめる。blastguard（コマンドに埋め込まれたプログラム。段階 S5）と検疫 plugin（ファイル内容）の両方がこの crate を使い、同じプログラムには同じ判定を出す。S5 の Python parser（`rustpython-parser` 0.4、`default-features = false` + `num-bigint`。malachite は LGPL なので使わない）もこの crate に作る。
4. **[決定]** 判定不能 — IO 失敗、parse 不能、未知の言語、hash 記録が読めない、plugin 未導入 — は必ず Ask か Deny に解決する（CLAUDE.md §3）。Ask を出せない場面（subagent）では `interactive::ask_available` に従って Deny にする（SUBSET Table C の subagent 列と同じ規則）。
5. **[決定]** テストは実装者以外が書き、RED を観測してから GREEN にする（CLAUDE.md §2 (a)(b)）。
6. **[決定]** blastguard にファイル読み取りやスクリプト内容の解析を足さない（memory `script-quarantine-plugin.md`「don't add file-reading/script-content analysis to blastguard」）。blastguard の責務外の処理は blastguard に入れない（memory `blastguard-scope-only-command-safety.md`）。

## 1. 現状（観測値）と前提段階

### 1.1 観測値

**[観測]** 2026-11-17、blastguard **0.2.679**（`~/.claude/plugins/cache/yukineko/blastguard/0.2.679/bin/blastguard`）。
観測は main session が relay `bgr-4507411e7461` で行った。stdin に PreToolUse の hook JSON を渡し
（`{"tool_name":"Bash","tool_input":{"command":"<C>"},"cwd":"/Users/yuki/src/.harness-worktrees/session-1e000416","hook_event_name":"PreToolUse",...}`）、
終了コードと stdout を直接読んだ。

| `<C>` | rc | stdout | 意味 |
|---|---|---|---|
| `./run.sh` | 0 | 空 | Allow |
| `bash run.sh` | 0 | 空 | Allow |
| `. ./env.sh` | 0 | 空 | Allow |
| `source env.sh` | 0 | 空 | Allow |
| `python3 x.py` | 0 | 空 | Allow |
| `scripts/foo.sh` | 0 | 空 | Allow |
| `bash <scratchpad>/run.sh`（実在。中身 `echo hi`） | 0 | 空 | Allow |
| `python3 <scratchpad>/x.py`（実在） | 0 | 空 | Allow |
| 対照: `chmod 777 /etc/hosts` | — | `"permissionDecision":"deny"` | Deny（観測手段は機能している） |
| 対照: `ls` | 0 | 空 | Allow |

**結論（事実）**: 稼働中の blastguard 0.2.679 は、B.5 の形をどれも Ask にしていない。
「いまは blastguard が Ask を返す」という依頼の前提は、稼働版の挙動と食い違っていた（Q1。§9 で解消済み）。
**現状は Allow（観測済み）であり、B.5 の Ask 化は S4 で入る（P0 = S4）。**

**[観測]** 依頼者（harness-5a）の 2026-11-17 の返信によると、稼働版 0.2.679 は S1 の型を凍結しただけで、detect には未配線である。
今の Allow は S2 の RED テストで固定されている。branch `bg/s2-red-20261117` の commit `ceed4a0db`、
`crates/blastguard/tests/ast_effect_rules_20261117.rs` の B4 群（`b4_bash_script_file_restricts` `bash script.sh`、
`b4_sh_script_file_restricts` `sh ./script.sh`、`b4_python3_script_file_restricts` `python3 x.py`、
`b4_perl_script_file_restricts` `perl x.pl`、wrapper 行 `env bash script.sh`）で、いずれも `#[ignore = "RED: S4 B.5 script-file row …"]` が付いている。
`. file` と `source file` は同じファイルの `#[ignore = "RED: S4 B.5 . file Ask (ruling 13)"]` と `"RED: S4 B.5 source file Ask (ruling 13)"` に含まれる。
この 2 点は main session が `git show ceed4a0db` で確認した。
直接実行の形（`./run.sh`、`scripts/foo.sh`）の RED は、同じファイルの中に見当たらなかった（Q2 は未解決のまま）。

**[観測]** この食い違いは文書の上でも説明がつく。SUBSET の冒頭に
「Specification only: … nothing here is wired into `detect` yet and no verdict changes in S1.」とあり、
B.5 の script-file 行を実装する段階は SUBSET「Stage map」節で **S4 `stdin-program`** とされている。
`judge.rs` の `judge` は S1 stub で、常に `Judgement::Undetermined(Cause::NotImplemented)` を返す。どこからも呼ばれていない。
ただし、稼働版の Allow がこの「未配線」だけで説明できるかは**未確認**である（legacy の string 層がこれらの形をどう読んでいるかは追っていない）。

**[観測]** `./run.sh` と `scripts/foo.sh`（interpreter を書かずにファイルを直接実行する形）は、`mod.rs` の `prog::program_source` では
`family(basename(argv0))` が `None` で、`eval`/`source`/`.` でも wrapper でもないので、`None`（program source なし）になる。
SUBSET の B.5 が例に挙げるのは interpreter 経由の形（`bash x.sh`、`python3 x.py`、`perl x.pl`）だけである。
したがって S1 の仕様の文面どおりに読むと、直接実行する形は「効果モデルの無い未知プログラム → Allow」に落ちる可能性がある（**[論点 Q2]**）。

### 1.2 前提段階（検疫 plugin より先に済ませる）

- **P0 = S4（Q1 解消、依頼者 harness-5a 2026-11-17）** blastguard が B.5 の script-file 行を Ask（subagent では Deny）にする。これは SUBSET の S4 の一部で、受け入れ条件は S2 の RED（上記 B4 群と `. file` / `source file`）が GREEN になることである。
  P0 が GREEN になる前に検疫 plugin を有効化しても意味がない。blastguard が既に Allow を返しているコマンドを、検疫が「Allow に下げる」ことはないからである。
  P0 の範囲に直接実行の形を含めるかは Q2 で決める。
- **P0 の帰結（明示）**: P0 を入れた時点から検疫 plugin が有効になるまでのあいだ、すべての `bash x.sh` / `python3 x.py` 等が Ask になる。
  Ask の量が増えるのは裁定 1 の帰結で、許容された状態とみなす（memory: 「plugin absent (stay Ask = fail-closed)」）。
  ただしこの期間の長さは運用上の判断になる（**[論点 Q3]**）。

## 2. 責務境界と連携方法

### 2.1 責務

| 主体 | 責務 | 持たないもの |
|---|---|---|
| blastguard | コマンド文字列の判定（SUBSET Table A/B/C）。B.5 で program source が `File(Value)` の行を Undetermined（Ask）にする。検疫に問い合わせた結果を、**B.5 の該当行の Ask を Allow に下げる**ことにだけ使う | ファイルの読み取り、言語の判別、ファイル内容の parse、hash 記録の管理 |
| script-quarantine（新 plugin） | 対象ファイルの解決結果（パス）を受け取り、読み取る・言語を判別する・parse する・判定する・hash を記録する。人間承認の受付（§7） | コマンド文字列そのものの判定（コマンドの他の部分は blastguard が判定する） |
| 共有 lib crate（§8） | 言語ごとの parser、効果の抽出、Table C 相当の分類と判定 | IO を伴う記録ストア、hook の入出力 |

**[提案]** 方向は approve.rs と同じく **Ask → Allow の一方向だけ**とする。検疫の結果が blastguard の Deny を下げることはない。
コマンドの他の部分（同じ行の他の simple command、redirect）の判定にも影響しない。
B.5 行の Ask だけが、検疫 Allow を条件に Allow へ下がる。

### 2.2 連携方式（**[論点 Q4]**）

blastguard と検疫は、どちらも Bash の PreToolUse で動く。連携方式の候補は次の 3 つ。

- **(A) 両方が独立した PreToolUse hook を持つ。** blastguard は B.5 行を Ask にし、検疫は Allow / Ask / Deny を出す。最終判定は Claude Code の合成規則に任せる。
  - Claude Code が複数 plugin の PreToolUse 判定をどう合成するか（たとえば deny > ask > allow の優先か、並列実行か）は **未確認**（repo 内に記述が見つからない）。
  - もし「最も制限的な判定が勝つ」なら、blastguard の Ask が検疫の Allow に勝つので、(A) では自動通過が実現しない。
- **(B) blastguard が検疫 binary を subprocess で呼ぶ。** `script-quarantine check --path <resolved> --lang-hint <interpreter> ...` の終了ステータスと stdout を判定に使う（CLAUDE.md §3: 終了ステータスを必ず使う）。
  - binary が見つからない、spawn できない、異常終了した、出力を parse できない場合はすべて Undetermined → Ask。plugin 未導入もここに含まれる。
  - 前例: GLOSSARY の「ask の非対話 hardening」項は、他の gate が判定を必要とするときは移植せずに blastguard binary に問い合わせる設計を記している。
  - 未決: blastguard が検疫 binary のパスをどう見つけるか（`installed_plugins.json` を読む、固定パス、環境変数）。
- **(C) blastguard が共有 lib crate を in-process で呼ぶ。** これは blastguard がファイルを読むことになり、裁定 6 に反する。plugin の有無とも無関係になり、裁定 2（「別 plugin が検疫する」）の形とも合わない。**[提案] 採らない。**

**[提案] 推奨は (B)。** 理由: 判定の順序が決定論的になり、hook 合成規則（未確認）に依存しない。plugin 未導入を「binary が見つからない → Ask」として構造的に表現できる。blastguard 側に増えるのは問い合わせと結果の解釈だけである。
(B) を採る場合でも、検疫 plugin が自前の PreToolUse hook を持つかは別途決める（§6.4 の TOCTOU 対策に hook が要るかどうかで変わる）。

### 2.3 plugin 未導入時

**[決定]** 検疫 plugin が未導入なら、B.5 行は Ask のまま（subagent では Deny）。
**[提案]** (B) では「binary を解決できない」を Undetermined として扱う。「plugin が無いことが判明した」として fail-soft に倒さない（GLOSSARY fail-soft 項: spawn 失敗は「無い」ではなく「わからない」側）。どちらにしても結論は Ask で同じなので、区別は理由文の中だけで行う。
**[論点 Q5]** plugin を無効化した後に、残っている hash 記録を blastguard が引き続き尊重してよいか。(B) では binary が呼ばれないので尊重されず、Ask になる。

## 3. 対象となる起動の形と対象ファイルの解決

### 3.1 対象の形

**[提案]** blastguard の `prog::program_source` が `ProgramSource::File(Value)` を返す形だけを対象にする。解析の入口を 2 つ作らないためである。

| 形 | 対象 | 備考 |
|---|---|---|
| `bash x.sh` / `sh x.sh`（オプション付きを含む。オプションの扱いは `shell_source` のとおりで、未モデルのオプションは既に Undetermined） | 対象 | 言語は sh/bash に固定（shebang は bash にとってはコメント） |
| `python3 x.py` / `python x.py`（`python_source` のとおり） | 対象 | 言語は Python に固定 |
| `zsh x.zsh` / `dash` / `ksh` | **[論点 Q6]** | brush-parser は bash の文法。SUBSET A.1 の ZshOnly 検出が file 内容にも十分かどうかは未検証。推奨: 初期は Ask（未対応言語） |
| `perl x.pl` / `ruby` / `node` | Ask（未知言語） | parser が無い。裁定 4 の「perl/ruby/node は subset 外」に合わせる |
| `. file` / `source file` | **[提案]** S4 の範囲 | 現在 `program_source` は `Undetermined`（「`source` runs shell code: not modelled」）。S4 が `File(Value)` を返すよう変えた場合に限り対象にする。実行は現在の shell（Bash ツールの shell。この環境では zsh。Claude Code が Bash ツールでどの shell を使うかは未確認）の中で、状態（cwd・変数）を共有する点が別 process の実行と違う |
| `./run.sh`、`scripts/foo.sh`、絶対パスの直接実行 | **[論点 Q2]** | 現在は B.5 の対象外（§1.1）。含める場合、言語は shebang から決める（§4.1） |
| PATH 上の名前（`mytool`） | 対象外 | 効果モデルの無い未知プログラム（裁定 2: Allow）。これを変えるのは本仕様の範囲外 |
| `python3 -m mod` | Ask | module の解決は sys.path に依存し、静的に決まらない（B.5 python 行）。将来の論点 |
| wrapper 経由（`env bash x.sh`、`timeout 5 python3 x.py`） | B.3 で unwrap したあと上の行に従う | SUBSET B.5 最終行 |

### 3.2 パスの解決

- **[決定]** 静的に決まるパスだけを対象にする。決まらなければ Ask。
- **[提案]** パスは blastguard が既に持つ `ProgramSource::File(Value)` を使い、検疫は再解決しない。`Value::Undetermined` → Ask（検疫を呼ばない）。
  相対パスは `eval::resolve_at(&cmd.cwd, …)` で解決する。cwd が Undetermined（undetermined な `cd` の後）なら Ask。
- **[提案]** 検疫の側では、解決済みパスに対して次を行う。
  - `canonicalize` して実パスを得る。symlink は実体の内容を検疫し、記録にも実パスを持たせる（approve.rs の「解決済み実パス」と同じ考え方）。
  - 通常ファイルでない（ディレクトリ、FIFO、device、存在しない）→ Ask。
  - 読み取れない → Ask。
- **[提案]** `bash x.sh` で、x.sh が cwd に無いときに bash が PATH を探す挙動は扱わない。cwd で解決したパスにファイルが無ければ Ask とする。
- **[提案]** スクリプトに渡す引数（`bash x.sh a b`）は判定に使わない。スクリプト内の `$1` 等は SUBSET A.3 で value Undetermined のままにする。こうすると記録の key に引数を含めずに済む。

### 3.3 S4（同じコマンド内で書いたファイル）との関係

- **[決定]**（SUBSET ruling 10）同じコマンドの前の部分が、決まった本文でそのファイルを書く場合（`cat > x.sh <<'EOF' … EOF; bash x.sh`）、その本文は blastguard が S4 で再帰的に判定する。検疫は関わらない（本文はコマンド文字列の中にある）。
- **[提案]** 同じコマンドの前の部分がそのファイルを**変えうる**のに、本文が決まらない場合は、検疫を呼ばずに Ask とする。例:
  - Table B で効果先が同じ解決パスになる、または Unresolved になる書き込み（`sed -i`、`cp y x.sh`、`echo … > "$f"`）
  - 効果モデルの無い未知プログラム（`./gen && bash x.sh` の `./gen`）。これを含めるかは **[論点 Q7]**。含めると、`make && bash out.sh` のような日常的な形が Ask になる。含めないと、PreToolUse の時点で検疫した内容と実行時の内容が違いうる。
- **[提案]** 検疫の判定（§5）は、**ファイルがディスクに存在する前提**の経路だけを扱う。ファイルがまだ無い（同じコマンドの前の部分で作られる予定）場合は、S4 で本文が決まらない限り Ask。

## 4. 言語の判別と言語ごとの parser

### 4.1 言語の判別

**[提案]** 次の順で判別し、決まらなければ Ask（**[決定]** 未知の言語は Ask）。

1. interpreter を書いて起動した形（`bash x.sh`、`python3 x.py`）: **interpreter が言語を決める**。shebang・拡張子は見ない（bash/python は shebang 行をコメントとして読み飛ばす）。
2. 直接実行の形（Q2 で対象とした場合）: shebang だけで決める。`#!/bin/sh`、`#!/bin/bash`、`#!/usr/bin/env bash`、`#!/usr/bin/env python3` を既知とする。
   - `#!/usr/bin/env -S …` や、未知の interpreter・shebang 行の引数（`#!/bin/bash -e` のオプションは shell_source と同じ集合に限る）→ Ask。
   - shebang が無い → Ask（execve の ENOEXEC 後に呼び出し側の shell が何で実行するかは shell ごとに違う）。
   - 拡張子は使わない。
3. NUL バイトを含む、UTF-8 として読めない → Ask。

### 4.2 parser

| 言語 | parser | 判定の流れ |
|---|---|---|
| sh / bash | `brush-parser` `=0.4.0`（blastguard の Cargo.toml と同じ pin、`default-features = false`） | SUBSET Table A（parse → eval）→ Table B（効果抽出）→ Table C（判定）。コマンド文字列と**同じ関数**に通す |
| Python | `rustpython-parser` 0.4（**[決定]** `default-features = false` + `num-bigint`、malachite は不可） | S5 で定義する Python の効果抽出（`os.remove`、`shutil.rmtree`、`open(…, "w")`、`subprocess.run([...])` などを Table B の効果へ写す表）→ Table C |

- **[提案]** Python の効果抽出の表は、S5 で SUBSET に節として追加する（例: Table B.6 として）。検疫用に別表は作らない。この仕様は表を持たない。
- **[提案]** shell の `sh` と `bash` の方言差（POSIX sh モード）は、初期は区別しない。`brush-parser` の bash 文法で読む。sh として実行されるファイルに bash 拡張があっても、実行時にエラーになるだけで効果を増やさない、という主張は**未検証**なので、RED テスト（§10 T-L4）で確かめる。

### 4.3 source / import の扱い

**sh/bash の `source f` / `. f`:**
- **[提案]** 引数がスラッシュを含む静的なパスなら、そのファイルを再帰的に検疫する。相対パスの基準は、実行時の cwd（スクリプトのディレクトリではない）とする。
- **[提案]** スラッシュを含まない名前（`source lib.sh`）は Ask。bash は PATH を先に探すため、静的に決まらない。
- **[提案]** 動的な引数（`source "$(dirname "$0")/lib.sh"`）は Ask。これは実在するスクリプトでよく見る形なので、Ask が多く出ることが予想される（測定は §10 の計測項目）。
- **[提案]** 再帰の上限: 深さ 8、ファイル数 32（どちらも提案値）。超えたら Ask。循環は visited 集合で検出し Ask。

**Python の `import`:**
- **[論点 Q8]** 推奨案は次のとおり。
  - (i) スクリプトと同じディレクトリ（sys.path[0]）で解決できるローカル module は、再帰的に検疫する。
  - (ii) 標準ライブラリの module は、S5 の効果抽出表がモデル化した呼び出しだけを効果として扱う。
  - (iii) それ以外（第三者パッケージ、動的 import、`importlib`、`__import__`、`exec`/`eval`）は Undetermined → Ask。
- (iii) を Allow にする（「未知プログラム → Allow」の類推）と、import 先のコードがファイル内に無いまま実行されるので、裁定 10 の趣旨（本文がコマンドに無いものは Undetermined）に反する。推奨は Ask。
- 補足: 既存の blastguard `detect/interpreter/python_semantics.rs` の doc comment は「a module whose code is not in the text」を Ask としている。

**記録との関係:** source / import で読んだファイル全部の hash を、記録の key に含める（§6.2）。

## 5. 安全判定

- **[決定]** 判定規則は SUBSET の表をそのまま参照する。複製しない。
  - shell の構文の扱い → SUBSET **Table A**（A.1〜A.5）。
  - 効果と対象の抽出 → SUBSET **Table B**（B.1 ファイル操作、B.2 リダイレクト、B.3 wrapper、B.4 git、B.5 interpreter）。ファイルの中にある `bash y.sh` も B.5 に従って再帰する。
  - パス区分 × 効果 → 判定 → SUBSET **Table C**、および「Undetermined handling」節（Undetermined は Ask、TooLong/TooDeep は Deny、panic は Undetermined）。
- **[提案]** 判定の結果は `judge.rs` の `Judgement` の 4 値をそのまま使う。検疫が記録するのは `Judgement::Allow` だけである。Ask / Deny / Undetermined は記録せず、blastguard へ「Allow ではない」と返す。その結果、B.5 行は Ask のままになる。
  - 検疫が Deny を返した場合に、blastguard が B.5 行を Deny に上げるかは **[論点 Q9]**。推奨は「上げる」。system harm を見つけたのに Ask にとどめるのは、memory `system-harm-always-deny.md` に反する。
- **[提案]** legacy の string 層（SUBSET「Undetermined handling」2〜3）は、ファイル内容には適用しない。legacy の層はコマンド文字列用で、ファイル内容に当てると誤検知の再生産になる。この点は S6 の方針と整合する必要がある（**[論点 Q10]**）。
- **[提案]** Table C は PathCtx（session の cwd、自分の worktree、Scratch）に依存する。したがって**同じ内容でも、どこで実行するかで判定が変わる**。§6.2 の key 設計はこれを扱う必要がある。

### 5.1 上限

| 項目 | 値（**[提案]**、Q11 で確定） | 超えたとき |
|---|---|---|
| 1 ファイルのサイズ | 256 KiB | **[論点 Q11]** Ask（推奨）か Deny（SUBSET A.1 の TooLong → Deny に合わせる）か |
| 構文の入れ子の深さ | SUBSET A.1 の `MAX_PARSE_DEPTH`（16）と同じ値 | Deny（A.1 と同じ） |
| eval の work budget | SUBSET A.2 の `MAX_WORK` / `MAX_COMMANDS` と同じ値 | Unsupported → Ask |
| source / import の closure | 深さ 8、ファイル数 32、合計 1 MiB | Ask |

- 注意: SUBSET A.1 の `MAX_PARSE_BYTES`（8 KiB）はコマンド文字列向けの値で、ファイルにそのまま当てると、ほぼすべての実スクリプトが Deny になる。共有 crate で上限を「入力種別ごと」に持たせる必要がある（§8）。

## 6. hash 記録

### 6.1 hash 方式

- **[提案]** SHA-256 を使う（`sha2` は workspace 依存。approve.rs が `sha256_hex` で使っている）。ファイルの生バイト列に対して計算する。改行コードや BOM を正規化しない（正規化すると、hash が同じでも実行内容が違う状態を作りうる）。

### 6.2 記録の key

**[提案]** key = 次の連結の SHA-256。

1. 言語（`sh` / `bash` / `python`）
2. **規則表の版**: 共有 crate が公開する定数 `RULESET_VERSION`。SUBSET の表、`classify` の述語、parser の版のどれかが変わったら上げる。
   - 上げ忘れを防ぐため、**[論点 Q12]** として「共有 crate の package version をそのまま key に入れる」案（どの変更でも全記録が無効になる。厳しい側）を推奨する。
3. 対象ファイルの内容 hash
4. source / import で辿った各ファイルの（実パス、内容 hash）を実パス順に並べたもの
5. **判定文脈**: Table C が PathCtx に依存するので、次のどちらかが必要になる（**[論点 Q13]**）。
   - (a) key に PathCtx の要素（解決済みの実行 cwd、自分の worktree の root、Scratch の root 集合）を入れる。厳しいが、worktree ごとに記録が分かれて再利用が減る。
   - (b) 記録には「効果と対象の一覧（Finding 列。相対パスは相対のまま）」を保存し、通過のたびに Table C の分類を今の PathCtx でやり直す。parse は省けるが、「同じ hash は自動で通す」（裁定 2）を「同じ hash は parse を省く」と読み替えることになる。
   - 推奨: (b)。理由: 自分の worktree の中に効く `rm -rf build` を、別の場所で実行したときにも通してしまう事故を構造的に防げる。ただし裁定 2 の文言との整合は人間が確認する必要がある。

記録には key のほかに、実パス、検疫した時刻、判定の内訳（Finding 列）、記録した主体（`auto` / `human`、§7）を持たせる。
**[提案]** approve.rs と同じく、記録は自分が置かれている key を名乗る。名乗った key とファイル名が一致しない記録は Undetermined として扱う。

### 6.3 置き場所・形式・書き込み主体

- **[提案]** 置き場所: `~/.script-quarantine/records/<key>.json`。1 記録 1 ファイルで、index は作らない（approve.rs の「置き場所」節と同じ理由: 並行セッションで lock の失敗が全 read を Undetermined に落とすのを避ける）。
  - 環境変数 `SCRIPT_QUARANTINE_DIR` で上書きできるようにする。目的は利便ではなく、テストの対照（別ストアに向けたらまた Ask になる。approve.rs の対照 v-a に相当）を書くことである。
- **[提案]** 書き込み: 一意な tmp 名（pid + 乱数）に書いてから `rename` する。
  - 注意: approve.rs の `write_atomic` は tmp 名が `path.with_extension("tmp")` で固定なので、同じ key に同時に書くと tmp が衝突しうる。この形は複製しない。
  - 同じ key への同時書き込みは、内容が同じ（決定論的な判定）なら後勝ちでよい。rename は原子的なので、半端なファイルが見えることはない。
- **[提案]** 書けるのは検疫 binary だけである。agent が直接書いた記録を「安全」と読ませないため、次を行う。
  - (i) blastguard の保護パス（`exclude::is_protected_path`）に記録ディレクトリを加える（blastguard 側の変更が要る）。
  - [観測] rev `6024bd3a3` の `crates/blastguard/src/exclude.rs` を grep しても、`.blastguard/` 系の保護パターンは見当たらなかった。approve.rs のストア（`~/.blastguard/approvals/`）が他の仕組みで守られているかは**未確認**。この点は approve.rs にも同じ問いがある。
  - (ii) 記録の改ざん検出（署名・HMAC）は、鍵も agent が読める場所にあるので実効性が低い。**[提案] 採らない**（Q14 で確認）。
- **[論点 Q14]** 記録ディレクトリの保護を blastguard の Protected 行（Table C: Ask / Deny）に入れるか、System 相当（常に Deny）にするか。

### 6.4 規則変更時の無効化

- **[提案]** key に規則表の版（§6.2-2）を入れるので、版が上がれば古い記録は参照されなくなり、自動的に無効になる。古いファイルの掃除は別の CLI（`script-quarantine gc`）で行い、判定の正しさには関わらない。
- 人間承認の記録（§7）が規則の版の変更で無効になるかは、Q15 に含める。

### 6.5 検疫〜実行の間の内容変化（TOCTOU）

PreToolUse の時点で hash を計算してから、Bash ツールが実際にファイルを読むまでのあいだに、内容が変わりうる（並行セッション、バックグラウンドのプロセス、同じコマンドの前の部分）。

候補は次のとおり（**[論点 Q16]**）。

- (a) **内容アドレスのコピーを実行させる**: 検疫済みの内容を `~/.script-quarantine/blobs/<hash>` に保存し、PreToolUse の `updatedInput` でコマンドを書き換える。
  - Claude Code の PreToolUse で `updatedInput` が使えるかは **未確認**。
  - 書き換えると `$0` / `BASH_SOURCE` / `__file__` / Python の sys.path[0] が変わり、スクリプトのディレクトリを基準にする相対参照が壊れる。
  - コマンドの書き換えは blastguard のコマンド判定とも順序の問題を起こす。
- (b) **残余リスクとして明記する**: PreToolUse 時の hash で判定し、TOCTOU の窓を残す。そのうえで、同じコマンド内での変更は §3.3 で Ask にして窓を狭める。
- (c) (b) に加えて、PostToolUse で同じファイルを再 hash し、記録の hash と違えば警告と記録を出す（事後検出。実行は防げない。スクリプトが自分自身を書き換える正当なケースとは区別できない）。

**推奨: (b) + (c)。** 厳しい側は (a) だが、未確認の機能に依存し、スクリプトの意味も変える。残る穴: 「PreToolUse から実行までのあいだに他プロセスが書き換える」は防げない。

## 7. parser で判定できないファイルを人間が承認する手段（未解決）

- 状況: parse はできたが Undetermined / Ask になったファイル（動的な source、未モデルのオプション、第三者 import など）や、未対応の言語のファイルを、人間が「このファイルは OK」と承認したい。
- **[提案]** 承認は記録の `recorded_by = human` として、§6 と同じ key 方式で保存する。内容が 1 バイト変われば無効になる。
- **[論点 Q15]** 未解決の点:
  1. **承認の入口**: approve.rs と同様に「Ask に人間が yes と答え、ツールが実際に走った（PostToolUse）」を承認とみなすか。それとも明示的な CLI（`script-quarantine approve <path>`）にするか。
     - PostToolUse 方式は、B.5 の Ask に yes と答えるたびに暗黙に記録される。その結果、同じ内容は次から聞かれなくなる。approve.rs と同じ TOFU で、日常の Ask の繰り返しは消える。一方で、人間が「今回だけ」のつもりで答えた yes まで記憶される。
     - CLI 方式では、agent 自身が CLI を呼べてしまうので、blastguard がその呼び出しを Ask / Deny にする規則が要る。agent が自分で承認を作る経路は塞がなければならない。
  2. **承認の範囲**: 規則の版が上がったときに人間承認も無効にするか。推奨は「無効にしない」。人間は規則ではなく内容を承認したからである。ただし、内容が同じでも新しい規則なら Deny になる場合（system harm）は、承認より Deny を優先する。
  3. **Deny（system harm）のファイルを人間承認で通せるか。** 推奨は「通せない」（memory `system-harm-always-deny.md`）。
  4. 承認の失効（期限）を持たせるか。

## 8. 共有 lib crate

### 8.1 名前の案（**[論点 Q17]**）

- `effectscope`（効果 × 対象の判定が主題）
- `progjudge`（プログラムの判定）
- `harness-progscan`（`harness-` 接頭辞で非 plugin の共有 crate であることを示す）

推奨: `effectscope`。どれも未使用であることは `ls crates/` で確認した（rev `6024bd3a3`）。

### 8.2 構成（**[提案]**）

```
crates/effectscope/
  Cargo.toml            # 非 plugin の lib crate（plugin.json / marketplace.json を持たない）
  src/lib.rs
  src/shell/            # brush-parser 0.4.0 + Table A の parse/eval（blastguard の detect/ast から移設）
  src/python/           # rustpython-parser 0.4 + S5 の効果抽出
  src/judge.rs          # Effect / PathClass / Finding / Judgement / judge / classify（SUBSET Table B/C）
  src/limits.rs         # 入力種別ごとの上限（コマンド文字列 / ファイル）
  src/version.rs        # RULESET_VERSION
```

- IO は持たない（純粋関数）。ファイルの読み取り、記録ストア、hook の入出力は利用側に置く。
- SUBSET.md は共有 crate に移す。blastguard 側にはポインタだけを残す（表の複製を作らない）。

### 8.3 blastguard から移すもの

| 対象（rev `6024bd3a3` で実在を確認） | 移す | 障害 |
|---|---|---|
| `detect/ast/{parse,eval,arith,support}.rs`、`eval/`、`mod.rs` の型（`Script` / `SimpleCommand` / `Value` / `ProgramSource` / `StdinSource` …） | 移す | `eval.rs` は 2000 行の上限に近い（`mod.rs` の `prog` module の注記）。分割が要る可能性がある |
| `detect/ast/judge.rs`（`Effect` / `PathClass` / `Judgement` / `judge` / `classify`） | 移す | `PathClass` の述語が blastguard の内部（`SafeRoots`、`exclude::is_protected_path`、`exclude::is_workflow_path`、`harm_mark::harms_system`、`protected::real_system_path_block`、`launch_agents_new_file_ask`）に依存している。(i) 述語も一緒に移すか、(ii) `PathCtx` に trait として注入するか（**[論点 Q18]**）。推奨は (ii)。述語の正典を blastguard に残すと、ゲートの保護リスト（exclude.rs）を共有 crate 側で二重管理せずに済む |
| `detect/ast/SUBSET.md` | 移す | specguard の binding（`.specguard/spec-docs.toml`、`docs/specs/blastguard.md`）も張り替える |
| `detect/ast/targets.rs`（legacy の resolve 呼び出し） | 移さない | S6 で退役する層 |
| `detect/interpreter/python_*.rs`（文字列ベースの Python 判定） | 移さない | S5 で共有 crate の Python parser に置き換え、S6 で退役させる |
| approve.rs | 移さない | blastguard の verdict を変える機能（memory の inventory で keep） |

### 8.4 S3〜S6 に対する移行順序（**[提案]**、Q19）

1. **S3 path-cutover** は blastguard の中で完了させる（移設は S3 の GREEN の後）。S3 の最中に crate の境界を動かすと、RED/GREEN の証跡と移設の差分が混ざるからである。
2. **M1（新設）**: 純粋移動で `detect/ast` を共有 crate へ移す。1 commit = 1 module の純粋移動とし、`refactor` skill の証明ツールで挙動の保存を示す。blastguard は共有 crate に依存する。
3. **S4 stdin-program**: 共有 crate の上で実装する（B.5 の shell 行。P0 = script-file 行の Ask を含む）。
4. **S5 python-parser**: 共有 crate に Python parser を作る（**[決定]** 作る場所）。
5. **Q1〜Q5 の裁定後に検疫 plugin を実装する**（§11）。S5 の前に sh/bash だけで始めてもよい。
6. **S6 retire-readings**: 検疫とは独立。

## 9. 未解決論点（人間の裁定が要るもの）

番号は本文の **[論点 Qn]** に対応する。推奨はあくまで推奨で、決定ではない。

1. **Q1 前提の食い違い — 解消済み（依頼者 harness-5a、2026-11-17）**: 今は Allow であることを観測で確かめた（§1.1）。P0 は S4 の一部として入れ、S2 の RED（`ceed4a0db`）で固定済み。前倒しはしない。
2. **Q2 直接実行の形**: `./run.sh`、`scripts/foo.sh`、絶対パスの直接実行を B.5 の対象（Ask → 検疫）に含めるか。含めると言語は shebang で決める。含めないと、検疫を経ずに Allow のまま残る。
3. **Q3 移行期間**: P0 から検疫の有効化までのあいだに Ask が増えることを、どれだけの期間許容するか。P0 と検疫を同時に有効化するか。
4. **Q4 連携方式**: (A) 独立した hook、(B) blastguard が検疫 binary を subprocess で呼ぶ、(C) in-process。推奨は (B)。(A) は hook の合成規則（未確認）に依存する。(B) の場合、binary のパスをどう見つけるか。
5. **Q5 plugin 無効化後の記録**: plugin を無効化した後に、残っている記録を尊重するか。推奨は「尊重しない（Ask）」。
6. **Q6 zsh/dash/ksh**: zsh/dash/ksh のファイルを brush-parser（bash の文法）で読んで判定してよいか。推奨は、初期は Ask。
7. **Q7 前段の未知プログラム**: 同じコマンドで前に未知プログラムがある場合（`make && bash out.sh`）、ファイルが変わりうるとして Ask にするか。
8. **Q8 Python import**: ローカル module は再帰、標準ライブラリは効果表、それ以外は Ask、という推奨案でよいか。
9. **Q9 検疫 Deny の伝播**: 検疫が Deny（system harm を含む）を出したとき、blastguard の B.5 行を Deny に上げるか。推奨は「上げる」。
10. **Q10 legacy 層の不適用**: legacy の string 層をファイル内容に適用しないことでよいか。
11. **Q11 ファイルサイズ上限**: 上限の値と、超えたときに Ask にするか Deny にするか。
12. **Q12 規則の版**: key に入れる規則の版を、手で上げる `RULESET_VERSION` にするか、crate の version（厳しい側）にするか。
13. **Q13 判定文脈**: 記録の key に PathCtx を含めるか（a）、Finding 列を保存して毎回 Table C を引き直すか（b、推奨）。(b) は裁定 2 の「自動で通す」の読み替えになる。
14. **Q14 記録ディレクトリの保護**: 記録ディレクトリを blastguard の保護パスに入れるか。その強さ（Protected 行か常に Deny か）と、署名を採らないことの確認。approve.rs のストアも同じ問いを持つ。
15. **Q15 人間承認**: 入口（PostToolUse の TOFU か CLI か）、規則の版が上がったときの扱い、Deny を承認で通せるか、期限。
16. **Q16 TOCTOU**: 推奨は (b)+(c)（残余リスクを明記し、事後検出を足す）。(a) は `updatedInput`（未確認）に依存する。
17. **Q17 crate 名**: 推奨は `effectscope`。
18. **Q18 述語の置き場所**: 推奨は、PathClass の述語を trait として注入し、正典を blastguard に残すこと。
19. **Q19 移行順序**: §8.4 の順序（S3 は移設前に完了、M1 純粋移動、S4/S5 は共有 crate の上で実装）でよいか。
20. **Q20 GATE crate 指定**: 検疫 plugin を GATE crate にするか（§10）。共有 crate の version bump をゲートでどう強制するか。
21. **Q21 Claude Code の合成規則**: 複数の PreToolUse hook の判定がどう合成されるか（優先順位、並列か直列か）は**未確認**。誰がどう確かめるか（公式文書の確認、実測）。結果によって Q4 の (A) の可否が変わる。
22. **Q22 `updatedInput`**: Claude Code の PreToolUse が `updatedInput` を受け付けるかは**未確認**。Q16 の (a) の可否が変わる。

## 10. GATE crate、rollout と有効化の順序、テスト計画

### 10.1 GATE crate にするか（**[論点 Q20]**）

- **[提案]** 検疫 plugin は判定（Allow の根拠）を持つので、GATE crate にする。
  - GATE crate にすると、`scripts/rollout-plugins.sh` の `GATE_CRATES=` と、`scripts/check-gate-crates-sync.py` の SOURCES に登録された全コピー（`.githooks/pre-push`、`scripts/continuous-audit.sh`、`scripts/check-plugin-rollout.py`、`scripts/check-fail-open-mutation.py`、`crates/harness-core/src/fleet.rs`、`crates/overwatch/skills/continuous-audit/SKILL.md`、`docs/OVERVIEW.md` ほか）を同じ commit で揃える必要がある。rollout では canary が必須になる（`docs/repo-operations.md`「GATE クレートは canary 必須」）。
- **[提案]** 共有 crate は非 plugin なので GATE crate の対象外になる。ただし `scripts/check-version-bumped.py` の `SHARED_CRATE = "harness-core"` は harness-core しか見ないので、共有 crate の bump-on-change を強制するよう拡張する必要がある（Q20 に含める）。共有 crate の変更は blastguard と検疫 plugin の両方の判定を変えるので、bump が無いと配布された版から判定規則を特定できなくなる。

### 10.2 rollout と有効化の順序（**[提案]**）

1. 共有 crate の移設（M1）を、blastguard の挙動を変えずに出荷する（blastguard の canary rollout）。
2. P0（B.5 の Ask 化）を blastguard の canary で出荷する。この時点から B.5 の形は Ask になる。
3. 検疫 plugin を `scripts/parked-plugins.json` に理由つきで parked として登録し、配布はするが有効化しない。この状態でも blastguard は Ask のまま（binary の解決に失敗 → Ask）で、fail-closed が保たれていることを確認する。
4. 検疫 plugin を canary で有効化し、blastguard の問い合わせ経路を有効にする。
5. Ask の件数（B.5 由来）と、検疫 Allow による自動通過の件数を、blastguard の block log（`~/.blastguard/blocks/blocks.jsonl`）で測る。

### 10.3 テスト計画（RED の一覧）

**[決定]** 実装者以外の Agent が書き、実装前（stub に対して）に RED を観測してから GREEN にする。
fail-closed のケース（§3 の判定不能すべて）には **(fc)** を付ける。対照実験は approve.rs の `tests/approval_memory.rs` の形に倣う。

**前提段階（blastguard）**
- T-P0-1 `bash x.sh`（実在、中身 `echo hi`）→ main では Ask、subagent では Deny。現在 0.2.679 では Allow（§1.1）なので RED になる。
- T-P0-2 `python3 x.py` → 同上。
- T-P0-3 (Q2 の裁定次第) `./run.sh` → 同上。
- T-P0-4 対照: `cat > x.sh <<'EOF'\necho hi\nEOF\nbash x.sh` → S4 の再帰判定で Allow（検疫は呼ばない）。
- T-P0-5 (fc) `bash "$f"` → Ask（Value Undetermined、検疫を呼ばない）。
- T-P0-6 (fc) undetermined な `cd` の後の `bash x.sh` → Ask。

**連携（Q4 = (B) を前提）**
- T-C1 (fc) 検疫 binary が無い → B.5 行は Ask（plugin 未導入）。
- T-C2 (fc) 検疫 binary が非 0 で終わる、または stdout が空・parse できない → Ask。
- T-C3 (fc) 検疫 binary が panic する、または timeout する → Ask。
- T-C4 検疫 Allow → B.5 行だけ Allow。同じ行の他の部分の Deny（`bash x.sh; chmod 777 /etc/hosts`）は Deny のまま。
- T-C5 検疫 Allow があっても、blastguard の Deny は下がらない。
- T-C6 (Q9 次第) 検疫 Deny → B.5 行が Deny（system harm フラグを保持）。

**言語と parse**
- T-L1 (fc) 未知の言語（`perl x.pl`）→ Ask。
- T-L2 (fc) 直接実行で shebang が無い、`env -S`、未知の interpreter → Ask。
- T-L3 (fc) UTF-8 でない、NUL を含む → Ask。
- T-L4 sh として実行されるファイルに bash 拡張がある → 判定が記録されたとおりであること（§4.2 の未検証主張の確認）。
- T-L5 (fc) parse エラー → Ask。サイズ上限超過 → Q11 の値。深さ超過 → Deny。
- T-L6 (fc) parser の panic → Ask（catch_unwind）。

**判定の一致（裁定 3: 同じプログラムには同じ判定）**
- T-E1 同じ shell 本文を、`bash -c '<本文>'`（blastguard、S4）と `bash x.sh`（検疫）に通すと、Judgement が一致する。Table C の非 legacy の各セルについて行う。
- T-E2 同じ Python 本文を、`python3 -c`（blastguard、S5）と `python3 x.py`（検疫）に通すと一致する。
- T-E3 system harm（`rm -rf /`、`chmod 777 /etc/hosts`）を含むファイル → Deny。記録されない。

**source / import**
- T-S1 静的な `. ./lib.sh` → 再帰して判定する。lib.sh 側の Deny は全体の Deny になる。
- T-S2 (fc) `source lib.sh`（スラッシュなし）、`source "$(dirname "$0")/x"` → Ask。
- T-S3 (fc) source の循環、深さ・ファイル数の上限超過 → Ask。
- T-S4 lib.sh を書き換えると、親ファイルの記録が無効になって再判定される。
- T-S5 (fc) (Q8 次第) 第三者 import、`importlib`、`exec` → Ask。

**hash 記録**
- T-R0 baseline: 記録なしの初回 → 検疫して Allow + 記録が 1 件できる（これが無いと以下の「聞かない」が空虚になる）。
- T-R1 同じ内容の 2 回目 → parse せずに Allow（parser の呼び出し回数で観測する）。
- T-R2 1 バイト変更 → 再検疫。
- T-R3 規則の版を上げる → 再検疫。
- T-R4 別のストア（`SCRIPT_QUARANTINE_DIR`）に向ける → 再検疫（T-R1 が本当にストアを読んでいる証拠）。
- T-R5 (fc) 記録が壊れている（parse できない）、自分の key を名乗らない記録、ストアの IO 失敗 → Ask（**[決定]** 記録が読めない → Ask）。
- T-R6 (fc) 記録ディレクトリが読めない（権限）→ Ask。
- T-R7 同じ key に同時に書く（N プロセス）→ 半端な記録が見えない。どれか 1 つが残る。
- T-R8 (Q13 = (b) の場合) 同じ内容を別の cwd で実行 → Table C を引き直し、その場所の判定になる。
- T-R9 (fc) agent が記録ディレクトリに Write / `echo > …` で書く → blastguard が Ask / Deny（Q14）。
- T-R10 Ask / Deny / Undetermined の判定は記録されない。

**TOCTOU**
- T-T1 (fc) `sed -i s/a/b/ x.sh && bash x.sh` → Ask（§3.3）。
- T-T2 (Q7 次第) `./gen && bash x.sh` → Ask。
- T-T3 (Q16 の (c) を採る場合) 実行後に内容が変わったことを PostToolUse で検出し、記録に残す。

**計測（テストではなく観測）**
- M-1 実在するスクリプト（この repo の `scripts/*.sh`）を検疫したとき、Allow / Ask / Deny が何件になるか。動的な source・`$HOME`・引数の多用によって Ask が多数になる予想（§4.3）を、数字で確かめる。数字には測定コマンド・rev・日付を併記する。

## 11. 実装の順序（まとめ、**[提案]**）

Q1〜Q5、Q13、Q14、Q20 の裁定 → S3 完了 → M1（共有 crate への純粋移動）→ S4（P0 を含む）→ 検疫 plugin の sh/bash 版（parked で配布）→ canary で有効化 → S5（Python）→ 検疫 plugin の Python 対応。
