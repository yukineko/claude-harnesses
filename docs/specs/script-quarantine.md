# script-quarantine（スクリプト検疫）仕様 — draft

> **状態: draft（2026-11-17）。実装なし。** この文書は設計仕様であり、どの crate の挙動も変えない。
> 「未解決論点」（§9）に挙げたものは人間の裁定が出るまで決定ではない。推奨案を添えているが、推奨は決定ではない。
> **2026-11-17 のユーザー裁定（依頼者 harness-5a 経由）を反映した。** 裁定済みの論点は §9 で「裁定済み（2026-11-17）」とし、
> 改めてユーザーに聞く論点は §9.1、実測で確かめる論点は §9.2 にまとめた。
> **2026-11-17 の第 2 次ユーザー裁定（依頼者 harness-5a 経由。Q4-path / Q7 / Q9 / Q12 / Q14 / Q15）も反映した。** §9.1 の当初 5 件（Q4-path、Q7、Q9、Q14、Q15）はすべて裁定済みで、Q12 は裁定済みの内容を改訂した。
> 裁定の反映で新たに生じた未決点・食い違いは §9.1 に置いた。
> **2026-11-17 の第 3 次ユーザー裁定（依頼者 harness-5a 経由。D-1 / D-2 / D-3）も反映した。** §9.1 の D-1〜D-3 はすべて裁定済みである。
> Q12 の「上げ忘れはゲートで検出」のゲートの置き場所と判定方法だけは未定のまま残し、実装の段階で改めてユーザーに聞く（§9.1）。
> **2026-11-17 の第 4 次ユーザー裁定（依頼者 harness-5a 経由。E-1〜E-4）も反映した。** 第 3 次裁定の反映で仕様に足さなかった 4 点（検疫し直した結果の扱い、approvals ストアの schema 版、schema 版の定数の置き場所、常に Deny にする tool の範囲）への回答である（§9.1）。
> **2026-11-17 の第 5 次ユーザー裁定（依頼者 harness-5a 経由。F-1〜F-3）も反映した。** 第 4 次裁定の反映で §9.1 に残した食い違い 3 点（読めない記録と §0 の裁定 4、「記録が無効」に当たる範囲、判定できない tool の扱い）への回答である（§9.1）。

本文の各記述には次のラベルを付ける。

- **[決定]** — ユーザー裁定 2026-11-17 に由来する。この仕様の中では変更しない。
- **[観測]** — 測定コマンド・版・日付つきで観測した事実。
- **[提案]** — この仕様が置く案。実装前のレビューで覆してよい。
- **[論点 Qn]** — 人間の裁定が要る。§9 の番号を指す。
- **[裁定済み Qn]** — 2026-11-17 のユーザー裁定で決まった論点。中身は §9 の該当項目を正とする。
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
   - **[裁定済み Q13]** 「同じ hash は自動で通す」は、「Finding の列を保存し、実行のたびに今の PathCtx で Table C を引き直す」という意味に読み替える（§6.2）。
3. **[決定]** 言語ごとの parser と「効果 × 対象パス」の判定規則は、共有 lib crate 1 つにまとめる。blastguard（コマンドに埋め込まれたプログラム。段階 S5）と検疫 plugin（ファイル内容）の両方がこの crate を使い、同じプログラムには同じ判定を出す。S5 の Python parser（`rustpython-parser` 0.4、`default-features = false` + `num-bigint`。malachite は LGPL なので使わない）もこの crate に作る。
4. **[決定]** 判定不能 — IO 失敗、parse 不能、未知の言語、hash 記録が読めない、plugin 未導入 — は必ず Ask か Deny に解決する（CLAUDE.md §3）。Ask を出せない場面（subagent）では `interactive::ask_available` に従って Deny にする（SUBSET Table C の subagent 列と同じ規則）。
   - **[裁定済み F-1・F-2]（2026-11-17）** 「hash 記録が読めない」は、次の場合と同じく「記録が無い」として扱う: 記録が読めない、key が無い、ストアの IO が失敗した、ディレクトリが読めない。
     そのうえで今の中身を検疫し、検疫できたらその結果に従う。検疫そのものができなければ、main では Ask、subagent では Deny にする。新しい記録は、書けるときだけ書く。
     これで裁定 4 と E-1 は両立する（§6.2、§9.1）。
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
ただし追加裁定（2026-11-17）により、`. file` / `source file` の Ask 化だけは検疫 plugin の導入と同時になる（§1.2 の P0-b）。

**[観測]** 依頼者（harness-5a）の 2026-11-17 の返信によると、稼働版 0.2.679 は S1 の型を凍結しただけで、detect には未配線である。
今の Allow は S2 の RED テストで固定されている。branch `bg/s2-red-20261117` の commit `ceed4a0db`、
`crates/blastguard/tests/ast_effect_rules_20261117.rs` の B4 群（`b4_bash_script_file_restricts` `bash script.sh`、
`b4_sh_script_file_restricts` `sh ./script.sh`、`b4_python3_script_file_restricts` `python3 x.py`、
`b4_perl_script_file_restricts` `perl x.pl`、wrapper 行 `env bash script.sh`）で、いずれも `#[ignore = "RED: S4 B.5 script-file row …"]` が付いている。
`. file` と `source file` は同じファイルの `#[ignore = "RED: S4 B.5 . file Ask (ruling 13)"]` と `"RED: S4 B.5 source file Ask (ruling 13)"` に含まれる。
この 2 点は main session が `git show ceed4a0db` で確認した。
- **[決定]（追加裁定 2026-11-17、harness-5a 経由）** `. file` / `source file` を Ask にする時期は S4 ではなく、**検疫 plugin の導入と同時**とする（P0 = 検疫の導入時）。
  理由（裁定原文）: `. "$HOME/.cargo/env"` が subagent で Deny にならないようにするため。
- **食い違い（明記）**: `ceed4a0db` の ruling 13 の RED 2 件は、ignore 理由のラベルが `RED: S4 …` のままである。一方、受け入れの時期は検疫の導入時である。
  これを検疫の導入時に GREEN にするのか、ラベルを付け直すのかは harness-5a 側で扱う。この仕様では決めない。

直接実行の形（`./run.sh`、`scripts/foo.sh`）の RED は、`ceed4a0db` の同じファイルの中に見当たらなかった。
Q2 は裁定済み（直接実行を含める）で、その RED は harness-5a が S2 の追補として**追加予定（S2 追補）**である。commit hash はまだ届いていない。

**[観測]** この食い違いは文書の上でも説明がつく。SUBSET の冒頭に
「Specification only: … nothing here is wired into `detect` yet and no verdict changes in S1.」とあり、
B.5 の script-file 行を実装する段階は SUBSET「Stage map」節で **S4 `stdin-program`** とされている。
`judge.rs` の `judge` は S1 stub で、常に `Judgement::Undetermined(Cause::NotImplemented)` を返す。どこからも呼ばれていない。
ただし、稼働版の Allow がこの「未配線」だけで説明できるかは**未確認**である（legacy の string 層がこれらの形をどう読んでいるかは追っていない）。

**[観測]** `./run.sh` と `scripts/foo.sh`（interpreter を書かずにファイルを直接実行する形）は、`mod.rs` の `prog::program_source` では
`family(basename(argv0))` が `None` で、`eval`/`source`/`.` でも wrapper でもないので、`None`（program source なし）になる。
SUBSET の B.5 が例に挙げるのは interpreter 経由の形（`bash x.sh`、`python3 x.py`、`perl x.pl`）だけである。
したがって S1 の仕様の文面どおりに読むと、直接実行する形は「効果モデルの無い未知プログラム → Allow」に落ちる可能性がある。
**[裁定済み Q2]** 直接実行の形（`./run.sh`、`scripts/foo.sh`、絶対パス）も B.5 の対象に含める。中身がコマンド文に無ければ Undetermined として Ask にし、検疫済みの hash だけが自動で通る。
したがって S4 で `prog::program_source` が直接実行の形にも `ProgramSource::File(Value)` を返すようにする必要がある。言語は shebang で判別する（§4.1）。

### 1.2 前提段階（検疫 plugin より先に済ませる）

- **P0 = S4（Q1 解消、依頼者 harness-5a 2026-11-17）** blastguard が B.5 の script-file 行を Ask（subagent では Deny）にする。これは SUBSET の S4 の一部で、受け入れ条件は S2 の RED が GREEN になることである。
  P0 が GREEN になる前に検疫 plugin を有効化しても意味がない。blastguard が既に Allow を返しているコマンドを、検疫が「Allow に下げる」ことはないからである。
- **P0 の範囲は 2 つに分ける（追加裁定 2026-11-17）**:
  - **P0-a（S4 で入れる）**: `bash` / `sh` / `python3` / `perl` のファイル実行（上記 B4 群）と、直接実行の形（`./run.sh`、`scripts/foo.sh`、絶対パス。**[裁定済み Q2]**。RED は追加予定（S2 追補））。
  - **P0-b（検疫 plugin の導入と同時に入れる）**: `. file` / `source file`（`ceed4a0db` の ruling 13 の RED 2 件。ignore 理由のラベルが S4 のままである食い違いは §1.1 に記した）。
- **P0 の帰結（明示）**: P0-a を入れた時点から検疫 plugin が有効になるまでのあいだ、すべての `bash x.sh` / `python3 x.py` / `./run.sh` 等が Ask になる。
  Ask の量が増えるのは裁定 1 の帰結で、許容された状態とみなす（memory: 「plugin absent (stay Ask = fail-closed)」）。
  **[裁定済み Q3]** Ask が増える期間を許す。S4 を先に入れ、Ask の増加は replay で測る（§10.2、M-2）。

## 2. 責務境界と連携方法

### 2.1 責務

| 主体 | 責務 | 持たないもの |
|---|---|---|
| blastguard | コマンド文字列の判定（SUBSET Table A/B/C）。B.5 で program source が `File(Value)` の行を Undetermined（Ask）にする。検疫に問い合わせた結果を、**B.5 の該当行の Ask を Allow に下げる**ことと、**検疫 Deny のときに B.5 の該当行を Deny に上げる**こと（**[裁定済み Q9]**）にだけ使う | ファイルの読み取り、言語の判別、ファイル内容の parse、hash 記録の管理 |
| script-quarantine（新 plugin） | 対象ファイルの解決結果（パス）を受け取り、読み取る・言語を判別する・parse する・判定する・hash を記録する。人間承認の受付（§7） | コマンド文字列そのものの判定（コマンドの他の部分は blastguard が判定する） |
| 共有 lib crate（§8） | 言語ごとの parser、効果の抽出、Table C 相当の分類と判定 | IO を伴う記録ストア、hook の入出力 |

**[提案]** 下げる方向は approve.rs と同じく **Ask → Allow だけ**とする。検疫の結果が blastguard の Deny を下げることはない。
コマンドの他の部分（同じ行の他の simple command、redirect）の判定にも影響しない。
B.5 行の Ask だけが、検疫 Allow を条件に Allow へ下がる。
**[裁定済み Q9]** 上げる方向として、検疫が Deny と判定したら、blastguard の判定も Deny に上げる（§5）。

### 2.2 連携方式（**[裁定済み Q4]**: (B) subprocess。binary のパスの見つけ方は **[裁定済み Q4-path]**）

blastguard と検疫は、どちらも Bash の PreToolUse で動く。連携方式の候補は次の 3 つ。

- **(A) 両方が独立した PreToolUse hook を持つ。** blastguard は B.5 行を Ask にし、検疫は Allow / Ask / Deny を出す。最終判定は Claude Code の合成規則に任せる。
  - Claude Code が複数 plugin の PreToolUse 判定をどう合成するかは、2026-11-18 に実測した（Q21、§9.2）。2 つの plugin の hook は並列に走り、deny は allow に勝ち、ask も allow に勝つ（headless では ask は拒否になる）。
  - もし「最も制限的な判定が勝つ」なら、blastguard の Ask が検疫の Allow に勝つので、(A) では自動通過が実現しない。
- **(B) blastguard が検疫 binary を subprocess で呼ぶ。** `script-quarantine check --path <resolved> --lang-hint <interpreter> ...` の終了ステータスと stdout を判定に使う（CLAUDE.md §3: 終了ステータスを必ず使う）。
  - binary が見つからない、spawn できない、異常終了した、出力を parse できない場合はすべて Undetermined → Ask。plugin 未導入もここに含まれる。
  - 前例: GLOSSARY の「ask の非対話 hardening」項は、他の gate が判定を必要とするときは移植せずに blastguard binary に問い合わせる設計を記している。
  - **[裁定済み Q4-path]** 検疫 plugin の install 先は `~/.claude/plugins/installed_plugins.json` から読む。plugin が有効かどうかは `enabledPlugins` で確かめる。
    見つからない、または読めないときは Ask（subagent では裁定 4 により Deny）。plugin が無効のときも検疫 binary は呼ばず、B.5 行は Ask のまま（§2.3）。
- **(C) blastguard が共有 lib crate を in-process で呼ぶ。** これは blastguard がファイルを読むことになり、裁定 6 に反する。plugin の有無とも無関係になり、裁定 2（「別 plugin が検疫する」）の形とも合わない。**[提案] 採らない。**

**[裁定済み Q4] (B) を採る（推奨どおり）。** 推奨時の理由: 判定の順序が決定論的になり、hook 合成規則（未確認）に依存しない。plugin 未導入を「binary が見つからない → Ask」として構造的に表現できる。blastguard 側に増えるのは問い合わせと結果の解釈だけである。
(B) を採る場合でも、検疫 plugin が自前の PreToolUse hook を持つかは別途決める（§6.4 の TOCTOU 対策に hook が要るかどうかで変わる）。

### 2.3 plugin 未導入時

**[決定]** 検疫 plugin が未導入なら、B.5 行は Ask のまま（subagent では Deny）。
**[提案]** (B) では「binary を解決できない」（**[裁定済み Q4-path]**: `installed_plugins.json` に見つからない・読めない、`enabledPlugins` で有効と確かめられない）を Undetermined として扱う。「plugin が無いことが判明した」として fail-soft に倒さない（GLOSSARY fail-soft 項: spawn 失敗は「無い」ではなく「わからない」側）。どちらにしても結論は Ask で同じなので、区別は理由文の中だけで行う。
**[裁定済み Q5]** plugin を無効化した後に残っている hash 記録は尊重しない（Ask）。(B) では binary が呼ばれないので尊重されず、Ask になる。

## 3. 対象となる起動の形と対象ファイルの解決

### 3.1 対象の形

**[提案]** blastguard の `prog::program_source` が `ProgramSource::File(Value)` を返す形だけを対象にする。解析の入口を 2 つ作らないためである。

| 形 | 対象 | 備考 |
|---|---|---|
| `bash x.sh` / `sh x.sh`（オプション付きを含む。オプションの扱いは `shell_source` のとおりで、未モデルのオプションは既に Undetermined） | 対象 | 言語は sh/bash に固定（shebang は bash にとってはコメント） |
| `python3 x.py` / `python x.py`（`python_source` のとおり） | 対象 | 言語は Python に固定 |
| `zsh x.zsh` / `dash` / `ksh` | Ask（**[裁定済み Q6]** 当面 Ask） | brush-parser は bash の文法。SUBSET A.1 の ZshOnly 検出が file 内容にも十分かどうかは未検証。初期は Ask（未対応言語） |
| `perl x.pl` / `ruby` / `node` | Ask（未知言語） | parser が無い。裁定 4 の「perl/ruby/node は subset 外」に合わせる |
| `. file` / `source file` | 対象（**[決定]** Ask 化は検疫 plugin の導入と同時。§1.2 P0-b） | 現在 `program_source` は `Undetermined`（「`source` runs shell code: not modelled」）。`File(Value)` を返すよう変えた場合に限り対象にする。実行は現在の shell（Bash ツールの shell。この環境では `/bin/zsh` 5.9 と実測した。§9.2）の中で、状態（cwd・変数）を共有する点が別 process の実行と違う |
| `./run.sh`、`scripts/foo.sh`、絶対パスの直接実行 | 対象（**[裁定済み Q2]**。S4 = P0-a） | 現在は B.5 の対象外（§1.1）で、S4 で `File(Value)` を返すようにする。言語は shebang で判別する（§4.1）。blastguard はファイルを読まない（裁定 6）ので、shebang を読むのは検疫の側である。interpreter が無いので `--lang-hint` は渡さない |
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
- **[裁定済み Q7]** 効果モデルの無い未知プログラムが前段にある形（`make && bash out.sh`、`./gen && bash x.sh`）は、一律に Ask にはしない。実行直前のファイル内容で検疫する。
  「前段のプログラムがファイルを書き換えた後の内容は見えない」という残余リスクは §9（Q7 の項）に明記した。
- **[提案]** 検疫の判定（§5）は、**ファイルがディスクに存在する前提**の経路だけを扱う。ファイルがまだ無い（同じコマンドの前の部分で作られる予定）場合は、S4 で本文が決まらない限り Ask。

## 4. 言語の判別と言語ごとの parser

### 4.1 言語の判別

**[提案]** 次の順で判別し、決まらなければ Ask（**[決定]** 未知の言語は Ask）。

1. interpreter を書いて起動した形（`bash x.sh`、`python3 x.py`）: **interpreter が言語を決める**。shebang・拡張子は見ない（bash/python は shebang 行をコメントとして読み飛ばす）。
2. 直接実行の形（**[裁定済み Q2]** で対象に含めた）: shebang だけで決める。`#!/bin/sh`、`#!/bin/bash`、`#!/usr/bin/env bash`、`#!/usr/bin/env python3` を既知とする。
   - `#!/usr/bin/env -S …` や、未知の interpreter・shebang 行の引数（`#!/bin/bash -e` のオプションは shell_source と同じ集合に限る）→ Ask。
   - shebang が無い → Ask（execve の ENOEXEC 後に呼び出し側の shell が何で実行するかは shell ごとに違う。この環境の Bash ツールの shell は `/bin/zsh` と実測した。§9.2）。
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
- **[裁定済み Q8]** 推奨案を採る。
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
  - **[裁定済み Q9]** 検疫が Deny と判定したら、blastguard の判定も Deny に上げる。
- **[提案]** legacy の string 層（SUBSET「Undetermined handling」2〜3）は、ファイル内容には適用しない。legacy の層はコマンド文字列用で、ファイル内容に当てると誤検知の再生産になる。この点は S6 の方針と整合する必要がある（**[裁定済み Q10]** 推奨どおり、legacy の string 層はファイル内容に適用しない）。
- **[提案]** Table C は PathCtx（session の cwd、自分の worktree、Scratch）に依存する。したがって**同じ内容でも、どこで実行するかで判定が変わる**。**[裁定済み Q13]** により、記録は Finding 列を持ち、実行のたびに今の PathCtx で Table C を引き直す（§6.2）。

### 5.1 上限

| 項目 | 値（1 ファイルのサイズは **[裁定済み Q11]**。それ以外は **[提案]**） | 超えたとき |
|---|---|---|
| 1 ファイルのサイズ | 256 KiB（**[裁定済み Q11]**） | Ask（**[裁定済み Q11]** 推奨どおり。Deny〈SUBSET A.1 の TooLong → Deny に合わせる案〉は採らない） |
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
2. **規則表の版**: **[裁定済み Q12]** crate の version は使わない。判定規則を変えたときだけ手で上げる定数 `RULESET_VERSION` を key に入れる。
   - 上げ忘れはゲートで検出する（§10.1）。ゲートの置き場所と判定方法は未定であり、実装の段階で改めてユーザーに聞く（§9.1）。
   - 記録の形式（schema）の版は `RULESET_VERSION` とは別に持つ（**[裁定済み D-1]**。下の「記録の形式」）。
   - 前回の版でこの項に書いていた「共有 crate の package version をそのまま key に入れる」は、判断で書いたものであり、この裁定で撤回した。
3. 対象ファイルの内容 hash
4. source / import で辿った各ファイルの（実パス、内容 hash）を実パス順に並べたもの

**判定文脈（[裁定済み Q13]: (b) を採る）**: Table C が PathCtx に依存するので、key には PathCtx を入れない。
記録には「効果と対象の一覧（Finding 列。相対パスは相対のまま）」を保存し、実行のたびに今の PathCtx で Table C を引き直す。
「同じ hash は自動で通す」（裁定 2）は、この意味に読み替える。すなわち、同じ hash で省けるのは parse と効果の抽出であり、Table C の判定は毎回やり直す。
引き直した結果が `Judgement::Allow` のときだけ B.5 行を Allow に下げる。Ask / Deny / Undetermined なら Allow ではない（§5）。

- 不採用の (a)（key に PathCtx の要素〈解決済みの実行 cwd、自分の worktree の root、Scratch の root 集合〉を入れる）は、worktree ごとに記録が分かれて再利用が減る。
- (b) を採る理由（推奨時の理由）: 自分の worktree の中に効く `rm -rf build` を、別の場所で実行したときにも通してしまう事故を構造的に防げる。

**記録の形式（Q13 の帰結）**: 記録の本体は Finding 列である。記録には key のほかに、Finding 列（Table C の引き直しの入力。相対パスは相対のまま、記録時の cwd で解決しない）、
実パス、検疫した時刻、記録した主体（`auto` / `human`、§7）、**schema 版**を持たせる。
**[提案]** Finding 列は共有 crate の型をそのまま直列化する。
**[裁定済み D-1]（2026-11-17）**: 形式の版は `RULESET_VERSION` とは別に持つ。記録ごとに schema 版を入れる。
知らない schema 版の記録や読めない記録は無効として扱い、検疫し直す。
**[裁定済み E-1]（2026-11-17）**: 記録が無効で検疫し直し、結果が Allow だったときは、その回から Allow にする。結果が Ask や Deny なら、その判定に従う。
- 前回の版では「判定不能なので Ask とし、検疫し直す」と書いていた。検疫し直した結果をその回の判定に使うかどうかは決めていなかったが、E-1 で決まった。
- schema 版の定数は、この記録ストアを持つ crate（検疫 plugin の crate）に置く（**[裁定済み E-3]**、§8.2）。
- 前回の版では、形式の版を key の 2（crate の package version）に含めるとしていた。Q12 の裁定で crate の version を使わなくなったので未決（D-1）としていたが、この裁定で決まった。
**[提案]** 記録を作るのは、記録時の PathCtx で `Judgement::Allow` になった場合だけとする（§5 の「記録するのは Allow だけ」を維持する）。
Finding 列を読めない記録も、上の D-1 と同じく無効として扱い、検疫し直す。判定は検疫し直した結果に従う（**[裁定済み E-1]**: Allow ならその回から Allow、Ask や Deny ならその判定）。
**[提案]** approve.rs と同じく、記録は自分が置かれている key を名乗る。
**[G-1]（2026-11-17、harness-5a の判断。ユーザーには聞いていない）**: 名乗った key とファイル名が一致しない記録は信用しない。「無効な記録」として、記録が無い場合と同じに扱い、検疫し直す（下の F-1・F-2 と同じ扱い）。理由: F-1・F-2 と E-1 のどちらに当てはめても、記録を信用する以外の読み方は fail-open にならず、信用する読み方だけが fail-open になる。
**[裁定済み F-1・F-2]（2026-11-17）**: 次の場合は、どれも「記録が無い」として扱う。
- 記録が読めない
- key が無い
- ストアの IO が失敗した
- ディレクトリが読めない

そのうえで今の中身を検疫し、検疫できたらその結果に従う（Allow ならその回から Allow、Ask や Deny ならその判定）。検疫そのものができなければ、main では Ask、subagent では Deny にする。
新しい記録は、書けるときだけ書く（記録が書けなくても、判定は検疫の結果に従う）。これで §0 の裁定 4 と E-1 は両立する。
- 前回の版では、自分の key を名乗らない記録を Undetermined とし、ストアの IO 失敗と記録ディレクトリが読めない場合を Ask としていた。この裁定で置き換えた。
- 名乗った key とファイル名が一致しない記録（key はあるが別の key を名乗っている）も、上の G-1 により同じく「記録が無い」として扱う（§9.1 で未決としていた点）。

### 6.3 置き場所・形式・書き込み主体

- **[提案]** 置き場所: `~/.script-quarantine/records/<key>.json`。1 記録 1 ファイルで、index は作らない（approve.rs の「置き場所」節と同じ理由: 並行セッションで lock の失敗が全 read を Undetermined に落とすのを避ける）。
  - 環境変数 `SCRIPT_QUARANTINE_DIR` で上書きできるようにする。目的は利便ではなく、テストの対照（別ストアに向けたらまた Ask になる。approve.rs の対照 v-a に相当）を書くことである。
- **[提案]** 書き込み: 一意な tmp 名（pid + 乱数）に書いてから `rename` する。
  - 注意: approve.rs の `write_atomic` は tmp 名が `path.with_extension("tmp")` で固定なので、同じ key に同時に書くと tmp が衝突しうる。この形は複製しない。
  - 同じ key への同時書き込みは、内容が同じ（決定論的な判定）なら後勝ちでよい。rename は原子的なので、半端なファイルが見えることはない。
- **[裁定済み Q14 / D-3]** 記録ディレクトリと approvals ストア（`~/.blastguard/approvals/`）に書けるのは、**そのストアを持つ plugin の hook プロセスだけ**である。
  approvals ストアは blastguard の hook が、検疫の記録は検疫 plugin の hook が書く。
  agent のツール呼び出しからの書き込みは、どちらのストアでも常に Deny にする。
  agent が直接書いた記録を「安全」と読ませないためである。
  - **[裁定済み E-4]（2026-11-17）** 常に Deny にする対象は、書き込みをするすべての tool である。Bash、Write、Edit、MultiEdit、NotebookEdit に加え、書き込むかどうか判定できない tool も含める。
    前回の版は対象を「Bash / Write / Edit」と書いていた。
  - **[裁定済み F-3]（2026-11-17）** 書き込むかどうか判定できない tool は、引数にストアのパスが現れて、ストアを指すと分かるときだけ Deny にする。
    それ以外は既存の判定に任せる（Undetermined なら Ask）。未知の tool をすべて Deny にはしない。
  - [観測] rev `6024bd3a3` の `crates/blastguard/src/exclude.rs` を grep しても、`.blastguard/` 系の保護パターンは見当たらなかった。したがってこの Deny は blastguard 側の変更として新たに入れる必要がある。
  - 前回の版で D-3 とした食い違い（Q14 の「書けるのは検疫 plugin 自身だけ」と、rev `5222ef741` で approvals ストアに書いているのが blastguard 自身〈`crates/blastguard/src/main.rs:997` の `harness_core::config::base_dir("blastguard").join("approvals")`、書き込みは `approve.rs` の `Store`〉であること）は、
    **[裁定済み D-3]（2026-11-17）** で書き込み主体を「そのストアを持つ plugin の hook プロセス」と定めたことで解消した。
  - **[裁定済み E-2]（2026-11-17）** schema 版は approvals ストア（blastguard の `~/.blastguard/approvals/`）の記録にも入れる。
    知らない schema 版の記録や読めない記録は、無効として扱い Ask にする（§10.3 T-R18）。
    F-1・F-2（記録が無い扱い → 検疫し直す）は検疫記録についての裁定であり、approvals ストアには及ばない。approvals には検疫し直す手順が無いので、無効な記録は Ask のままとする（**[G-2]** 2026-11-17、harness-5a が確認。ユーザーには聞いていない）。
    schema 版の定数は、そのストアを持つ crate（blastguard）に置く（**[裁定済み E-3]**、§8.2）。
  - **[提案]** 記録の改ざん検出（署名・HMAC）は、鍵も agent が読める場所にあるので実効性が低い。採らない。

### 6.4 規則変更時の無効化

- **[提案]** key に規則表の版（§6.2-2。**[裁定済み Q12]** 手で上げる `RULESET_VERSION`）を入れるので、版が上がれば古い記録は参照されなくなり、自動的に無効になる。古いファイルの掃除は別の CLI（`script-quarantine gc`）で行い、判定の正しさには関わらない。
- **[裁定済み Q15]** 規則の版が上がったら、記録はすべて無効になる（人間承認の記録〈§7〉も含む）。
- **[裁定済み D-2]（2026-11-17）** 記録に期限は付けない。中身と `RULESET_VERSION` が同じなら、承認はずっと有効である。
- **[裁定済み D-1 / E-1]** 知らない schema 版の記録や読めない記録は無効として扱い、検疫し直す。検疫し直した結果が Allow ならその回から Allow、Ask や Deny ならその判定に従う（§6.2）。
- **[裁定済み F-1・F-2]** 記録が読めない、key が無い、ストアの IO が失敗した、ディレクトリが読めない場合は「記録が無い」として扱い、今の中身を検疫してその結果に従う。検疫そのものができなければ main では Ask、subagent では Deny。新しい記録は書けるときだけ書く（§6.2）。
- **[裁定済み E-2]** approvals ストアの記録にも schema 版を入れる。知らない版や読めない記録は無効として扱い、Ask にする（§6.3）。

### 6.5 検疫〜実行の間の内容変化（TOCTOU）

PreToolUse の時点で hash を計算してから、Bash ツールが実際にファイルを読むまでのあいだに、内容が変わりうる（並行セッション、バックグラウンドのプロセス、同じコマンドの前の部分）。

候補は次のとおり（**[裁定済み Q16]**: 推奨どおり (b) + (c)）。

- (a) **内容アドレスのコピーを実行させる**: 検疫済みの内容を `~/.script-quarantine/blobs/<hash>` に保存し、PreToolUse の `updatedInput` でコマンドを書き換える。
  - Claude Code の PreToolUse は `updatedInput` を受け付け、書き換えた後のコマンドが実行されることを 2026-11-18 に実測した（Q22、§9.2）。(b)+(c) の裁定は変えない。
  - 書き換えると `$0` / `BASH_SOURCE` / `__file__` / Python の sys.path[0] が変わり、スクリプトのディレクトリを基準にする相対参照が壊れる。
  - コマンドの書き換えは blastguard のコマンド判定とも順序の問題を起こす。
- (b) **残余リスクとして明記する**: PreToolUse 時の hash で判定し、TOCTOU の窓を残す。そのうえで、同じコマンド内での変更のうち Table B で効果先が決まる書き込みは §3.3 で Ask にして窓を狭める（前段の未知プログラムは **[裁定済み Q7]** により Ask にしない）。
- (c) (b) に加えて、PostToolUse で同じファイルを再 hash し、記録の hash と違えば警告と記録を出す（事後検出。実行は防げない。スクリプトが自分自身を書き換える正当なケースとは区別できない）。

**[裁定済み Q16] (b) + (c) を採る（推奨どおり）。** 厳しい側は (a) だが、未確認の機能に依存し、スクリプトの意味も変える。残る穴: 「PreToolUse から実行までのあいだに他プロセスが書き換える」は防げない。
また **[裁定済み Q7]** により、同じコマンドの前段のプログラムがファイルを書き換えた後の内容は見えない（§9 の Q7 の項）。

## 7. parser で判定できないファイルを人間が承認する手段（**[裁定済み Q15]**）

- 状況: parse はできたが Undetermined / Ask になったファイル（動的な source、未モデルのオプション、第三者 import など）や、未対応の言語のファイルを、人間が「このファイルは OK」と承認したい。
- **[提案]** 承認は記録の `recorded_by = human` として、§6 と同じ key 方式で保存する。内容が 1 バイト変われば無効になる。
- **[裁定済み Q15]**:
  1. **承認の入口**: 人間が Ask の許可プロンプトで許可し、かつ実行が成功したときだけ、PostToolUse でその hash を記録する。
  2. **subagent**: subagent では許可できないので、記録は起きない。
  3. **Deny**: Deny は承認で通せない。
  4. **規則の版**: 規則の版（`RULESET_VERSION`、§6.2）が上がったら、記録はすべて無効になる（人間承認の記録も含む）。
- **[裁定済み D-2]（2026-11-17）**: 期限は付けない。中身と `RULESET_VERSION` が同じなら、承認はずっと有効である。
- **[裁定済み D-3]**: 人間承認の記録を書くのは、検疫 plugin の hook（PostToolUse）である。agent のツール呼び出しからの書き込みは Deny（§6.3）。
  - **[裁定済み E-4 / F-3]**: Deny の対象は、書き込みをするすべての tool（Bash、Write、Edit、MultiEdit、NotebookEdit）と、書き込むかどうか判定できない tool である（§6.3）。
    判定できない tool は、引数にストアのパスが現れて、ストアを指すと分かるときだけ Deny にする。それ以外は既存の判定に任せる（Undetermined なら Ask）（**[裁定済み F-3]**）。

## 8. 共有 lib crate

### 8.1 名前（**[裁定済み Q17]**: `effectscope`）

共有 crate の名前は `effectscope`（効果 × 対象の判定が主題）とする（推奨どおり）。以下、本文の「共有 crate」はこの crate を指す。

- 不採用の候補: `progjudge`（プログラムの判定）、`harness-progscan`（`harness-` 接頭辞で非 plugin の共有 crate であることを示す）。
- どれも未使用であることは `ls crates/` で確認した（rev `6024bd3a3`）。

### 8.2 構成（**[提案]**）

```
crates/effectscope/
  Cargo.toml            # 非 plugin の lib crate（plugin.json / marketplace.json を持たない）
  src/lib.rs
  src/shell/            # brush-parser 0.4.0 + Table A の parse/eval（blastguard の detect/ast から移設）
  src/python/           # rustpython-parser 0.4 + S5 の効果抽出
  src/judge.rs          # Effect / PathClass / Finding / Judgement / judge / classify（SUBSET Table B/C）
  src/limits.rs         # 入力種別ごとの上限（コマンド文字列 / ファイル）
  src/version.rs        # 記録の key に入れる RULESET_VERSION（Q12 裁定済み。判定規則を変えたときだけ手で上げる。crate の version は使わない）
```

- 記録の schema 版は `RULESET_VERSION` とは別に持つ（**[裁定済み D-1]**、§6.2）。
- **[裁定済み E-3]（2026-11-17）** schema 版の定数は、そのストアを持つ crate に置く。approvals ストアの schema 版は blastguard に、検疫記録の schema 版は検疫 plugin の crate に置く。共有 crate（`effectscope`）には置かない（この crate は記録ストアを持たない）。
- IO は持たない（純粋関数）。ファイルの読み取り、記録ストア、hook の入出力は利用側に置く。記録ストアに書けるのは、そのストアを持つ plugin の hook プロセスだけである（**[裁定済み D-3]**、§6.3）。
- SUBSET.md は共有 crate に移す。blastguard 側にはポインタだけを残す（表の複製を作らない）。

### 8.3 blastguard から移すもの

| 対象（rev `6024bd3a3` で実在を確認） | 移す | 障害 |
|---|---|---|
| `detect/ast/{parse,eval,arith,support}.rs`、`eval/`、`mod.rs` の型（`Script` / `SimpleCommand` / `Value` / `ProgramSource` / `StdinSource` …） | 移す | `eval.rs` は 2000 行の上限に近い（`mod.rs` の `prog` module の注記）。分割が要る可能性がある |
| `detect/ast/judge.rs`（`Effect` / `PathClass` / `Judgement` / `judge` / `classify`） | 移す | `PathClass` の述語が blastguard の内部（`SafeRoots`、`exclude::is_protected_path`、`exclude::is_workflow_path`、`harm_mark::harms_system`、`protected::real_system_path_block`、`launch_agents_new_file_ask`）に依存している。(i) 述語も一緒に移すか、(ii) `PathCtx` に trait として注入するか。**[裁定済み Q18]** 推奨どおり (ii)（注入）を採る。述語の正典を blastguard に残すと、ゲートの保護リスト（exclude.rs）を共有 crate 側で二重管理せずに済む |
| `detect/ast/SUBSET.md` | 移す | specguard の binding（`.specguard/spec-docs.toml`、`docs/specs/blastguard.md`）も張り替える |
| `detect/ast/targets.rs`（legacy の resolve 呼び出し） | 移さない | S6 で退役する層 |
| `detect/interpreter/python_*.rs`（文字列ベースの Python 判定） | 移さない | S5 で共有 crate の Python parser に置き換え、S6 で退役させる |
| approve.rs | 移さない | blastguard の verdict を変える機能（memory の inventory で keep） |

### 8.4 S3〜S6 に対する移行順序（**[裁定済み Q19]**: 推奨どおりこの順序を採る）

1. **S3 path-cutover** は blastguard の中で完了させる（移設は S3 の GREEN の後）。S3 の最中に crate の境界を動かすと、RED/GREEN の証跡と移設の差分が混ざるからである。
2. **M1（新設）**: 純粋移動で `detect/ast` を共有 crate へ移す。1 commit = 1 module の純粋移動とし、`refactor` skill の証明ツールで挙動の保存を示す。blastguard は共有 crate に依存する。
3. **S4 stdin-program**: 共有 crate の上で実装する（B.5 の shell 行。P0-a = script-file 行と直接実行の形の Ask を含む。`.`/`source` の P0-b は含まない。§1.2）。
4. **S5 python-parser**: 共有 crate に Python parser を作る（**[決定]** 作る場所）。
5. **Q1〜Q5 の裁定後に検疫 plugin を実装する**（§11）。S5 の前に sh/bash だけで始めてもよい。Q1〜Q5 は Q4-path を含めてすべて裁定済み（2026-11-17）。P0-b（`.`/`source` の Ask 化）は検疫 plugin の導入と同時に入れる。
6. **S6 retire-readings**: 検疫とは独立。

## 9. 論点と裁定（2026-11-17 のユーザー裁定を反映）

番号は本文の **[論点 Qn]** / **[裁定済み Qn]** に対応する。
裁定は依頼者 harness-5a がユーザー裁定として伝えてきたものである。「推奨どおり採用」とした項目は、この文書の推奨案の本文をそのまま書いた。
推奨が一意に決まらない Q、および依頼者の分類と本文が食い違う Q は「推奨どおり採用」とせず、§9.1 に移した。実測で確かめる論点は §9.2 に置いた。
§9.1 に移した 5 件（Q4-path、Q7、Q9、Q14、Q15）と、前回「推奨どおり」とした Q12 の見直しは、2026-11-17 の第 2 次ユーザー裁定（依頼者 harness-5a 経由）で裁定済みになった。
その反映で生じた D-1〜D-3 は、2026-11-17 の第 3 次ユーザー裁定（依頼者 harness-5a 経由）で裁定済みになった（§9.1）。
第 3 次裁定の反映で仕様に足さなかった 4 点は、2026-11-17 の第 4 次ユーザー裁定（依頼者 harness-5a 経由）で裁定済みになった（§9.1 の E-1〜E-4）。
第 4 次裁定の反映で残した食い違い 3 点は、2026-11-17 の第 5 次ユーザー裁定（依頼者 harness-5a 経由）で裁定済みになった（§9.1 の F-1〜F-3）。

1. **Q1 前提の食い違い — 解消済み（依頼者 harness-5a、2026-11-17）**: 今は Allow であることを観測で確かめた（§1.1）。P0 は S4 の一部として入れ、S2 の RED（`ceed4a0db`）で固定済み。前倒しはしない。
   - 追加裁定（2026-11-17）: `.` / `source` の Ask 化（P0-b）だけは S4 ではなく、検疫 plugin の導入と同時に入れる。理由は `. "$HOME/.cargo/env"` が subagent で Deny にならないようにするため（§1.2）。ruling 13 の RED の ignore 理由ラベルが S4 のままである食い違いは §1.1 に記した（扱いは harness-5a 側）。
2. **Q2 直接実行の形 — 裁定済み（2026-11-17）: 含める。** `./run.sh`、`scripts/foo.sh`、絶対パスでの直接実行も B.5 の対象にする。中身がコマンド文に無ければ Undetermined として Ask にし、検疫済みの hash だけが自動で通る。言語は shebang で決める（§4.1）。RED は追加予定（S2 追補）。
3. **Q3 移行期間 — 裁定済み（2026-11-17）**: Ask が増える期間を許す。S4 を先に入れ、Ask の増加は replay で測る。
4. **Q4 連携方式 — 裁定済み（2026-11-17）: 推奨どおり (B)（blastguard が検疫 binary を subprocess で呼ぶ）。**
   - **Q4-path binary の見つけ方 — 裁定済み（2026-11-17）**: 検疫 plugin の install 先は `~/.claude/plugins/installed_plugins.json` から読む。plugin が有効かどうかは `enabledPlugins` で確かめる。見つからない、または読めないときは Ask（§2.2）。
5. **Q5 plugin 無効化後の記録 — 裁定済み（2026-11-17）: 推奨どおり「尊重しない（Ask）」。**
6. **Q6 zsh/dash/ksh — 裁定済み（2026-11-17）: 推奨どおり、初期は Ask（当面 Ask）。**
7. **Q7 前段の未知プログラム — 裁定済み（2026-11-17）**: `make && bash out.sh` の形は、一律に Ask にはしない。実行直前のファイル内容で検疫する（§3.3）。
   - **残余リスク（明記）**: 前段のプログラムがファイルを書き換えた後の内容は見えない。検疫が見るのは PreToolUse の時点（実行直前）のファイル内容であり、同じコマンドの前段（`make`、`./gen` など）が実行中にファイルを書き換えた場合、実際に実行される内容は検疫していない。Q16 (c) の PostToolUse 再 hash は事後検出であり、実行は防げない（§6.5）。
8. **Q8 Python import — 裁定済み（2026-11-17）: 推奨どおり。** ローカル module は再帰、標準ライブラリは効果表、それ以外は Ask。
9. **Q9 検疫 Deny の伝播 — 裁定済み（2026-11-17）**: 検疫が Deny と判定したら、blastguard の判定も Deny に上げる（§2.1、§5）。
10. **Q10 legacy 層の不適用 — 裁定済み（2026-11-17）: 推奨どおり。** legacy の string 層をファイル内容に適用しない（§5 の [提案]）。
11. **Q11 ファイルサイズ上限 — 裁定済み（2026-11-17）: 256 KiB。超えたときは推奨どおり Ask。**
12. **Q12 規則の版 — 裁定済み（2026-11-17、第 2 次裁定で改訂）**: crate の version は使わない。判定規則を変えたときだけ手で上げる `RULESET_VERSION` を key に入れる。上げ忘れはゲートで検出する（§6.2、§10.1）。ゲートの置き場所と判定方法は未定で、実装の段階で改めてユーザーに聞く（§9.1）。
    - 記録の形式（schema）の版は `RULESET_VERSION` とは別に持つ（D-1 裁定済み、§9.1）。schema 版の定数は、そのストアを持つ crate に置く（E-3 裁定済み、§8.2）。
    - 前回の版は「推奨どおり crate の version（厳しい側）」とし、「§6.2 の推奨は一意に決まる」と判断して書いていた。これは判断であって裁定ではなかったので、この裁定で置き換えた。
13. **Q13 判定文脈 — 裁定済み（2026-11-17）: (b) を採る。** Finding の列を保存し、実行のたびに今の PathCtx で Table C を引き直す。「同じ hash は自動で通す」はこの意味に読み替える（§0 裁定 2、§6.2）。
14. **Q14 記録ディレクトリの保護 — 裁定済み（2026-11-17。書き込み主体は D-3、対象の tool は E-4 と F-3 で確定）**: 記録ディレクトリと approvals ストア（`~/.blastguard/approvals/`）への書き込みは、agent のツール呼び出しからは常に Deny にする。
    対象は書き込みをするすべての tool（Bash、Write、Edit、MultiEdit、NotebookEdit）と、書き込むかどうか判定できない tool である（E-4）。
    判定できない tool は、引数にストアのパスが現れて、ストアを指すと分かるときだけ Deny にする。それ以外は既存の判定に任せる（Undetermined なら Ask）。未知の tool をすべて Deny にはしない（F-3）。
    書けるのは、そのストアを持つ plugin の hook プロセスだけ（approvals ストアは blastguard の hook、検疫の記録は検疫 plugin の hook。§6.3、§9.1 の D-3）。
15. **Q15 人間承認 — 裁定済み（2026-11-17）**: 人間が Ask の許可プロンプトで許可し、かつ実行が成功したときだけ、PostToolUse でその hash を記録する。subagent では許可できないので、記録は起きない。Deny は承認で通せない。規則の版が上がったら、記録はすべて無効になる（§7）。
    - 前回の版の推奨「規則の版が上がっても人間承認は無効にしない」は、この裁定で採らないことになった。
    - 承認の期限は D-2 で裁定済み: 期限は付けない。中身と `RULESET_VERSION` が同じなら、承認はずっと有効である（§7、§9.1 の D-2）。
16. **Q16 TOCTOU — 裁定済み（2026-11-17）: 推奨どおり (b)+(c)**（残余リスクを明記し、事後検出を足す）。(a) は `updatedInput`（Q22。受け付けることは 2026-11-18 に実測済み、§9.2）に依存する。
17. **Q17 crate 名 — 裁定済み（2026-11-17）: `effectscope`。**
18. **Q18 述語の置き場所 — 裁定済み（2026-11-17）: 推奨どおり**、PathClass の述語を trait として注入し、正典を blastguard に残す。
19. **Q19 移行順序 — 裁定済み（2026-11-17）: 推奨どおり** §8.4 の順序（S3 は移設前に完了、M1 純粋移動、S4/S5 は共有 crate の上で実装）。
20. **Q20 GATE crate 指定 — 裁定済み（2026-11-17）: 推奨どおり**、検疫 plugin を GATE crate にする（§10.1）。共有 crate の version bump は `scripts/check-version-bumped.py` を拡張して強制する（§10.1 の [提案]）。
21. **Q21 Claude Code の合成規則 — 2026-11-18 に実測済み（§9.2）。**
22. **Q22 `updatedInput` — 2026-11-18 に実測済み（§9.2）。**

### 9.1 ユーザーに聞く論点

当初ここに置いた 5 件（Q4-path、Q7、Q9、Q14、Q15）は、すべて裁定済み（2026-11-17）である。あわせて Q12 を改訂した（ここに置いていなかったが、表に含める）。当初の論点としては §9.1 は空になった。

| Q | 状態 |
|---|---|
| Q4-path | 裁定済み（2026-11-17）。§9 の 4. |
| Q7 | 裁定済み（2026-11-17）。残余リスクは §9 の 7. |
| Q9 | 裁定済み（2026-11-17）。§9 の 9. |
| Q12 | 裁定済み（2026-11-17、改訂）。§9 の 12. |
| Q14 | 裁定済み（2026-11-17）。§9 の 14. |
| Q15 | 裁定済み（2026-11-17）。§9 の 15. |

第 2 次裁定を本文へ反映する中で、裁定の文面からは決まらない点が新たに出た（D-1〜D-3）。これらは 2026-11-17 の第 3 次ユーザー裁定（依頼者 harness-5a 経由）で裁定済みになった。

| id | 当初の未決点 | 状態 | 出どころ |
|---|---|---|---|
| D-1 | Finding 列の直列化形式の版を何で表すか。前回の版は crate の package version に含めていたが、Q12 で crate の version を使わなくなった。 | **裁定済み（2026-11-17）**: 形式の版は `RULESET_VERSION` とは別に持つ。記録ごとに schema 版を入れ、知らない版や読めない記録は無効として扱い、検疫し直す（§6.2、§6.4）。検疫し直した結果の扱いは E-1 で裁定済み。 | Q12、§6.2 |
| D-2 | 人間承認の記録に期限（失効）を持たせるか。 | **裁定済み（2026-11-17）**: 期限は付けない。中身と `RULESET_VERSION` が同じなら、承認はずっと有効である（§6.4、§7）。 | Q15、§7 |
| D-3 | Q14 の「書けるのは検疫 plugin 自身だけ」を approvals ストア（`~/.blastguard/approvals/`）にどう当てはめるか。rev `5222ef741` ではこのストアに書いているのは blastguard 自身（`crates/blastguard/src/main.rs:997`、`approve.rs` の `Store`）である。 | **裁定済み（2026-11-17）**: 書き込めるのは、そのストアを持つ plugin の hook プロセスだけである。approvals ストアは blastguard の hook が、検疫の記録は検疫 plugin の hook が書く。agent のツール呼び出しからの書き込みは、どちらのストアでも常に Deny（§6.3。対象の tool は E-4 と F-3 で裁定済み）。 | Q14、§6.3 |

第 3 次裁定の反映で仕様に足さなかった 4 点（E-1〜E-4）は、2026-11-17 の第 4 次ユーザー裁定（依頼者 harness-5a 経由）で裁定済みになった。

| id | 当初の未決点 | 状態 | 反映先 |
|---|---|---|---|
| E-1 | 記録が無効で検疫し直したとき、検疫し直した結果をその回の判定に使うか。 | **裁定済み（2026-11-17）**: 結果が Allow なら、その回から Allow にする。結果が Ask や Deny なら、その判定に従う。 | §6.2、§6.4、T-R5、T-R11、T-R14 |
| E-2 | schema 版を approvals ストア（`~/.blastguard/approvals/`）にも入れるか。 | **裁定済み（2026-11-17）**: 入れる。 | §6.3、§6.4、T-R18 |
| E-3 | schema 版の定数をどの crate に置くか。 | **裁定済み（2026-11-17）**: そのストアを持つ crate に置く。approvals ストアは blastguard、検疫記録は検疫 plugin の crate。 | §6.2、§6.3、§8.2 |
| E-4 | 常に Deny にする tool の範囲（前回の版は Bash / Write / Edit と書いていた）。 | **裁定済み（2026-11-17）**: 書き込みをするすべての tool。Bash、Write、Edit、MultiEdit、NotebookEdit に加え、書き込むかどうか判定できない tool も含める（判定できない tool の範囲は F-3 で裁定済み）。 | §6.3、§7、§9 の 14.、T-R9、T-R9b |

E-1 / E-4 の反映で本文の記述に出た食い違い 3 点（F-1〜F-3）は、2026-11-17 の第 5 次ユーザー裁定（依頼者 harness-5a 経由）で裁定済みになった。

| id | 当初の食い違い | 状態 | 反映先 |
|---|---|---|---|
| F-1 | §0 の裁定 4 は「hash 記録が読めない」を判定不能に挙げ、Ask か Deny に解決するとしている。E-1 では、読めない記録は無効として検疫し直し、結果が Allow ならその回から Allow になる。両者を両立すると読んでよいか。 | **裁定済み（2026-11-17）**: F-2 と合わせて回答。下の「F-1・F-2 の裁定」。 | §0 の裁定 4、§6.2、§6.4、T-R5、T-R6、T-R19 |
| F-2 | 自分の key を名乗らない記録（前回の版は Undetermined）、ストアの IO 失敗（T-R5）、記録ディレクトリが読めない（T-R6）が、E-1 の「記録が無効」に当たるか。前回の版はこれらを Ask としていた。 | **裁定済み（2026-11-17）**: F-1 と合わせて回答。下の「F-1・F-2 の裁定」。 | §6.2、§6.4、T-R5、T-R6、T-R19 |
| F-3 | E-4 の「書き込むかどうか判定できない tool」について、書き込み先がストアかどうかも判定できない場合、その tool の呼び出しをすべて Deny にするのか。 | **裁定済み（2026-11-17）**: 書き込むかどうか判定できない tool は、引数にストアのパスが現れて、ストアを指すと分かるときだけ Deny にする。それ以外は既存の判定に任せる（Undetermined なら Ask）。未知の tool をすべて Deny にはしない。 | §6.3、§7、§9 の 14.、T-R9、T-R9b、T-R20 |

**F-1・F-2 の裁定（2026-11-17）**: 次の場合は、どれも「記録が無い」として扱う。

- 記録が読めない
- key が無い
- ストアの IO が失敗した
- ディレクトリが読めない

そのうえで今の中身を検疫し、検疫できたらその結果に従う。検疫そのものができなければ、main では Ask、subagent では Deny にする。新しい記録は、書けるときだけ書く。これで §0 の裁定 4 と E-1 は両立する。

F-1・F-2 の反映で決まらなかった次の 2 点は、harness-5a が判断した（2026-11-17。どちらもユーザーには聞いていない。fail-open にならない側を採ったため）。

- **G-1**: 名乗った key とファイル名が一致しない記録（key はあるが、別の key を名乗っている。§6.2 の [提案]）は信用しない。「無効な記録」として、記録が無い場合と同じに扱い、検疫し直す。
- **G-2**: approvals ストアの無効な記録は、F-1・F-2 の対象外として Ask のままとする（§6.3、T-R18）。

残る未決点は、次の 1 件である。自分の判断では埋めず、**実装の段階で改めてユーザーに聞く**。

| 未決点 | 出どころ |
|---|---|
| Q12 の「`RULESET_VERSION` の上げ忘れはゲートで検出する」について、ゲートの置き場所（どの script / hook に置くか）と判定方法（「判定規則を変えた」をどう機械判定するか）。 | Q12、§6.2、§10.1、T-R13 |

### 9.2 実測で確かめる論点（2026-11-18 に実測）

| 論点 | 状態 | 結果で変わるもの |
|---|---|---|
| Q21 複数の PreToolUse hook（複数 plugin）の判定がどう合成されるか（優先順位、並列か直列か） | **実測済み**: 並列に走る。deny は allow に勝つ（どちらの plugin が deny を返しても同じ）。ask も allow に勝つ（headless では拒否になる）。下の M-3 の c0〜c4。 | Q4 の (A) の可否（現在は (B) を裁定済み。裁定は変えない） |
| Q22 Claude Code の PreToolUse が `updatedInput` を受け付けるか | **実測済み**: 受け付ける。書き換えた後のコマンドが実行され、元のコマンドは実行されない。2 つの plugin が両方書き換えると、片方だけが効く（どちらが効くかの規則は 1 回の観測では決まらない）。書き換えと deny が重なると deny になる。下の M-3 の c5〜c7。 | Q16 の (a) の可否（現在は (b)+(c) を裁定済み。裁定は変えない） |
| Bash ツールが使う shell | **実測済み（この環境）**: `/bin/zsh` 5.9。`/bin/zsh -c 'source <shell snapshot> … && eval '<コマンド>''` の形で起動される（`setopt NO_EXTENDED_GLOB NO_BARE_GLOB_QUAL` が先に入る）。ログインシェルの設定に従うのかは確かめていない。 | `. file` / `source file` の実行文脈（§3.1）、shebang の無い直接実行の扱い（§4.1） |

## 10. GATE crate、rollout と有効化の順序、テスト計画

### 10.1 GATE crate にするか（**[裁定済み Q20]**: 推奨どおり GATE crate にする）

- **[裁定済み Q20]** 検疫 plugin は判定（Allow の根拠）を持つので、GATE crate にする。
  - GATE crate にすると、`scripts/rollout-plugins.sh` の `GATE_CRATES=` と、`scripts/check-gate-crates-sync.py` の SOURCES に登録された全コピー（`.githooks/pre-push`、`scripts/continuous-audit.sh`、`scripts/check-plugin-rollout.py`、`scripts/check-fail-open-mutation.py`、`crates/harness-core/src/fleet.rs`、`crates/overwatch/skills/continuous-audit/SKILL.md`、`docs/OVERVIEW.md` ほか）を同じ commit で揃える必要がある。rollout では canary が必須になる（`docs/repo-operations.md`「GATE クレートは canary 必須」）。
- **[提案]** 共有 crate は非 plugin なので GATE crate の対象外になる。ただし `scripts/check-version-bumped.py` の `SHARED_CRATE = "harness-core"` は harness-core しか見ないので、共有 crate の bump-on-change を強制するよう拡張する必要がある（**[裁定済み Q20]** 推奨どおり拡張する）。共有 crate の変更は blastguard と検疫 plugin の両方の判定を変えるので、bump が無いと配布された版から判定規則を特定できなくなる。
- **[裁定済み Q12]** 記録の key には crate の version ではなく、判定規則を変えたときだけ手で上げる `RULESET_VERSION` を入れる（§6.2）。`RULESET_VERSION` の上げ忘れはゲートで検出する。記録の無効化の正しさは、crate の version bump の強制ではなく、このゲートに依存する。
  ゲートをどの script に置くか、「判定規則を変えた」をどう機械判定するかは未定であり、実装の段階で改めてユーザーに聞く（§9.1）。

### 10.2 rollout と有効化の順序（**[提案]**）

1. 共有 crate の移設（M1）を、blastguard の挙動を変えずに出荷する（blastguard の canary rollout）。
2. P0-a（B.5 の Ask 化のうち、`bash`/`sh`/`python3`/`perl` のファイル実行と直接実行の形。S4）を blastguard の canary で出荷する。この時点からこれらの形は Ask になる。`.`/`source`（P0-b）はまだ Ask にしない（追加裁定 2026-11-17、§1.2）。
3. 検疫 plugin を `scripts/parked-plugins.json` に理由つきで parked として登録し、配布はするが有効化しない。この状態でも blastguard は Ask のまま（binary の解決に失敗 → Ask）で、fail-closed が保たれていることを確認する。
4. 検疫 plugin を canary で有効化し、blastguard の問い合わせ経路を有効にする。**同時に P0-b（`. file` / `source file` の Ask 化）を入れる**（追加裁定 2026-11-17。`. "$HOME/.cargo/env"` が subagent で Deny にならないようにするため）。
5. Ask の件数（B.5 由来）と、検疫 Allow による自動通過の件数を、blastguard の block log（`~/.blastguard/blocks/blocks.jsonl`）で測る。
   - **[裁定済み Q3]** 2.〜4. のあいだの Ask の増加は replay で測る（M-2）。replay の入力と手順はこの仕様にはまだ書いていない。

### 10.3 テスト計画（RED の一覧）

**[決定]** 実装者以外の Agent が書き、実装前（stub に対して）に RED を観測してから GREEN にする。
fail-closed のケース（§3 の判定不能すべて）には **(fc)** を付ける。対照実験は approve.rs の `tests/approval_memory.rs` の形に倣う。

**前提段階（blastguard）**
- T-P0-1 `bash x.sh`（実在、中身 `echo hi`）→ main では Ask、subagent では Deny。現在 0.2.679 では Allow（§1.1）なので RED になる。
- T-P0-2 `python3 x.py` → 同上。
- T-P0-3 (**[裁定済み Q2]**) `./run.sh`、`scripts/foo.sh`、絶対パスでの直接実行 → 同上。RED は追加予定（S2 追補。harness-5a が追加中で、commit hash はまだ届いていない）。
- T-P0-1〜3 の例外（**2026-11-18 裁定 T1**）: git で tracked かつ未変更（work tree と index が clean で、同じコマンドの他の部分が書き込まない）のプログラムファイルには B.5 の意見を出さない（従来の判定が残る）。`python3 -m` と、git が答えられない場合は Undetermined のまま。`bash -n` / `sh -n`（noexec）も B.5 の意見なし（T3）。fd から読むプログラム（`bash <&'$fd'`）は Undetermined（T2）。（SUBSET.md ruling 22–24）
- T-P0-7 `. ./env.sh` / `source env.sh` → 検疫 plugin の導入と同時に Ask（subagent では Deny）。S4 の時点では Ask にしない（追加裁定、§1.2 P0-b）。`ceed4a0db` の ruling 13 の RED は ignore 理由ラベルが S4 のままで、受け入れの時期（検疫の導入時）と食い違う（§1.1。扱いは harness-5a 側）。
- T-P0-4 対照（**2026-11-18 裁定で訂正**）: 同じコマンドで書いた本体を S4 が再帰判定するのは、書き込みと実行が `&&` だけで繋がっているときに限る。`cat > x.sh <<'EOF' && bash x.sh\necho hi\nEOF` → Allow（検疫は呼ばない）。改行（`cat > x.sh <<'EOF'\necho hi\nEOF\nbash x.sh`）や `;` で繋いだ場合、また `false && cat > x.sh …; bash x.sh` は Undetermined（main では Ask、subagent では Deny）。（SUBSET.md ruling 20）
- T-P0-5 (fc) `bash "$f"` → Ask（Value Undetermined、検疫を呼ばない）。
- T-P0-6 (fc) undetermined な `cd` の後の `bash x.sh` → Ask。

**連携（**[裁定済み Q4]** = (B)）**
- T-C1 (fc) 検疫 binary が無い → B.5 行は Ask（plugin 未導入）。
- T-C2 (fc) 検疫 binary が非 0 で終わる、または stdout が空・parse できない → Ask。
- T-C3 (fc) 検疫 binary が panic する、または timeout する → Ask。
- T-C4 検疫 Allow → B.5 行だけ Allow。同じ行の他の部分の Deny（`bash x.sh; chmod 777 /etc/hosts`）は Deny のまま。
- T-C5 検疫 Allow があっても、blastguard の Deny は下がらない。
- T-C6 (**[裁定済み Q9]**) 検疫 Deny → blastguard の判定が Deny（main でも subagent でも Deny。Ask にとどまらない）。
- T-C7 (fc) (**[裁定済み Q4-path]**) `~/.claude/plugins/installed_plugins.json` が無い・読めない・parse できない、または検疫 plugin の entry が無い → 検疫 binary を呼ばず B.5 行は Ask（subagent では Deny）。
- T-C8 (fc) (**[裁定済み Q4-path]**) 検疫 plugin は install 済みだが `enabledPlugins` で無効、または `enabledPlugins` を読めない → 検疫 binary を呼ばず Ask。対照: 同じ install で有効にしたときは検疫が呼ばれる（呼び出しを観測する）。

**言語と parse**
- T-L1 (fc) 未知の言語（`perl x.pl`）、zsh/dash/ksh（**[裁定済み Q6]** 当面 Ask）→ Ask。
- T-L2 (fc) 直接実行で shebang が無い、`env -S`、未知の interpreter → Ask。
- T-L3 (fc) UTF-8 でない、NUL を含む → Ask。
- T-L4 sh として実行されるファイルに bash 拡張がある → 判定が記録されたとおりであること（§4.2 の未検証主張の確認）。
- T-L5 (fc) parse エラー → Ask。サイズ上限（256 KiB）超過 → Ask（**[裁定済み Q11]**）。深さ超過 → Deny。
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
- T-S5 (fc) (**[裁定済み Q8]**) 第三者 import、`importlib`、`exec` → Ask。

**hash 記録**
- T-R0 baseline: 記録なしの初回 → 検疫して Allow + 記録が 1 件できる（これが無いと以下の「聞かない」が空虚になる）。
- T-R1 同じ内容の 2 回目 → parse せず、保存した Finding 列から今の PathCtx で Table C を引き直して Allow（parser の呼び出し回数で観測する。**[裁定済み Q13]**）。
- T-R2 1 バイト変更 → 再検疫。
- T-R3 規則の版（`RULESET_VERSION`。**[裁定済み Q12]**）を上げる → 再検疫。人間承認の記録も無効になる（**[裁定済み Q15]**）。対照: 共有 crate の package version だけを上げ、`RULESET_VERSION` を据え置くと、記録は有効のまま（key に crate の version が入っていないことの証拠）。
- T-R13 (fc) (**[裁定済み Q12]**) 判定規則を変えたのに `RULESET_VERSION` を上げていない差分 → ゲートが検出して止める。対照: 同じ差分で `RULESET_VERSION` を上げると通る。ゲートの置き場所と検出方法は未定で、実装の段階で改めてユーザーに聞く（§9.1、§10.1）。
- T-R4 別のストア（`SCRIPT_QUARANTINE_DIR`）に向ける → 再検疫（T-R1 が本当にストアを読んでいる証拠）。
- T-R5 (fc) (**[裁定済み D-1 / E-1 / F-1・F-2]**) 記録が読めない（壊れていて parse できない）、key が無い、ストアの IO が失敗した → 「記録が無い」として扱われ、今の中身を検疫する（parser の呼び出しで観測する）。判定は T-R19 の 3 通りで観測する。
  - 前回の版では、自分の key を名乗らない記録とストアの IO 失敗は Ask としていた。F-1・F-2 の裁定で置き換えた。
- T-R6 (fc) (**[裁定済み F-1・F-2]**) 記録ディレクトリが読めない（権限）→ 「記録が無い」として扱われ、今の中身を検疫する。判定は T-R19 の 3 通りで観測する。
  - 前回の版では Ask としていた。F-1・F-2 の裁定で置き換えた。
- T-R19 (fc) (**[裁定済み F-1・F-2]**、**[G-1]**) 記録が読めない・key が無い・名乗った key とファイル名が一致しない（G-1）・ストアの IO が失敗した・ディレクトリが読めない、の 5 つそれぞれについて、次の 3 つを観測する（5 × 3 = 15 ケース）。key の不一致は、別の内容で Allow として記録したファイルを、検疫できない中身の key の名前で置く形で作る（記録を信用すると Allow になってしまう形なので、信用していないことを観測できる）。
  - 中身が安全 → Allow（記録が無い扱い → 検疫 → Allow。parser が呼ばれたことを観測する）。
  - 検疫できない中身（parse 不能、未知の言語など）→ main で Ask、subagent で Deny。
  - 記録が書けない（記録ディレクトリへの書き込みが失敗する）状態でも、判定は検疫の結果に従う（安全な中身なら Allow、検疫できない中身なら main で Ask / subagent で Deny）。書けなかった記録が残っていないことも観測する。
- T-R7 同じ key に同時に書く（N プロセス）→ 半端な記録が見えない。どれか 1 つが残る。
- T-R8 (**[裁定済み Q13]**) 同じ内容を別の cwd で実行 → Table C を引き直し、その場所の判定になる（例: 自分の worktree で Allow として記録した `rm -rf build` を、Table C で Allow にならない場所で実行すると Allow にならない）。
- T-R11 (fc) 記録の Finding 列を読めない → 記録は無効として扱われ、検疫し直す。判定は検疫し直した結果に従う（§6.2、**[裁定済み D-1 / E-1]**。T-R14 と同じ 3 通りを観測する）。
- T-R14 (fc) (**[裁定済み D-1 / E-1]**) 記録の schema 版が知らない値（未来の版、未定義の値）、schema 版のフィールドが無い、または schema 版を読めない → 記録は無効として扱われ、検疫し直す（parser の呼び出しで観測する）。判定は検疫し直した結果に従うことを、ファイルの中身を変えた 3 通りで観測する。
  - 検疫し直した結果が Allow になる中身 → **その回から** Allow（同じ呼び出しで Ask を挟まない）。
  - 検疫し直した結果が Ask になる中身 → Ask（subagent では Deny）。
  - 検疫し直した結果が Deny になる中身 → Deny（main でも Deny）。
  - 対照: 同じ記録の schema 版を既知の値に戻すと、T-R1 と同じく parse せずに Table C を引き直す。
- T-R16 (**[裁定済み D-2]**) 期限が無いことの確認: 検疫した時刻が古い記録（例: 時刻を数年前に書き換えた、または時計を進めた状態で読む）でも、中身と `RULESET_VERSION` が同じなら Allow（再検疫しない）。人間承認の記録（`recorded_by = human`）でも同じ。
- T-R12 (fc) **[裁定済み Q5]** plugin を無効化した後、残っている記録があっても B.5 行は Ask。
- T-R9 (fc) (**[裁定済み Q14 / D-3 / E-4]**) agent のツール呼び出しで記録ディレクトリ（`~/.script-quarantine/records/`）に書く → Write、Edit、MultiEdit、NotebookEdit、Bash（`echo … > …`、`cp`、`mv` など）のどれでも常に Deny（main でも Deny。Ask にならない）。
  書き込むかどうか判定できない tool の呼び出しは、引数にストアのパスが現れて、ストアを指すと分かるときだけ Deny（**[裁定済み E-4 / F-3]**。T-R20）。
- T-R9b (fc) (**[裁定済み Q14 / D-3 / E-4]**) agent のツール呼び出しで approvals ストア（`~/.blastguard/approvals/`）に Write、Edit、MultiEdit、NotebookEdit、Bash で書く → 常に Deny（main でも Deny。Ask にならない）。書き込むかどうか判定できない tool の呼び出しは、引数にストアのパスが現れて、ストアを指すと分かるときだけ Deny（**[裁定済み F-3]**。T-R20）。
- T-R20 (**[裁定済み F-3]**) 書き込むかどうか判定できない tool について、記録ディレクトリと approvals ストアのそれぞれで次の 2 つを観測する。
  - (fc) 引数にストアのパスがあり、ストアを指すと分かる → Deny（main でも Deny）。
  - 引数にストアのパスが無い → 既存の判定に従う（Undetermined なら Ask）。対照: 未知の tool がすべて Deny にはならないことを確かめる（同じ未知の tool で、ストアのパスを含まない呼び出しが Deny にならないことを観測する）。
- T-R9c 対照 (**[裁定済み D-3]**): 検疫 plugin の hook プロセスによる記録ディレクトリへの書き込みは成功する（T-R0 で記録が 1 件できることと合わせて、T-R9 の Deny が「何も書けない」ではないことを示す）。
- T-R9d 対照 (**[裁定済み D-3]**): blastguard の hook プロセスによる approvals ストアへの書き込みは許可され、成功する（`approve.rs` の `Store` による承認の記録が 1 件できる。T-R9b の Deny が「何も書けない」ではないことを示す）。
- T-R10 Ask / Deny / Undetermined の判定は記録されない。
- T-R18 (fc) (**[裁定済み E-2]**) approvals ストア（`~/.blastguard/approvals/`）の記録の schema 版が知らない値（未来の版、未定義の値）、schema 版のフィールドが無い、schema 版を読めない、または記録そのものを読めない → 記録は無効として扱われ、Ask になる（承認の記憶で Allow に下がらない。subagent では Deny）。
  対照: 同じ記録の schema 版を blastguard が知っている値に戻すと、承認の記憶が効く（`tests/approval_memory.rs` の形に倣う）。
**人間承認（**[裁定済み Q15]**）**
- T-H1 main で Ask の許可プロンプトに人間が許可し、実行が成功 → PostToolUse でその hash が `recorded_by = human` として記録される。次回の同じ内容は聞かれない。
- T-H2 (fc) 人間が許可したが実行が失敗（非 0 終了）→ 記録されない。
- T-H3 (fc) subagent での同じ形 → Ask が出せず Deny になるので許可が起きず、記録されない（記録件数が 0 のままであることを観測する）。
- T-H4 (fc) 人間が拒否した → 記録されない。
- T-H5 (fc) 検疫が Deny と判定したファイル → 承認の経路が無く通らない。承認の記録が既にあっても Deny（たとえば規則の変更で Deny になった内容）。

**TOCTOU**
- T-T1 (fc) `sed -i s/a/b/ x.sh && bash x.sh` → Ask（§3.3）。
- T-T2 (**[裁定済み Q7]**) `make && bash out.sh` / `./gen && bash x.sh` → 一律に Ask にはしない。実行直前（PreToolUse の時点）のファイル内容で検疫した結果になる（検疫 Allow なら Allow、検疫 Deny なら Deny）。前段が書き換えた後の内容は見えない残余リスク（§9 の 7.）はテストでは塞がない。
- T-T3 (**[裁定済み Q16]** (c)) 実行後に内容が変わったことを PostToolUse で検出し、記録に残す。

**計測（テストではなく観測）**
- M-1 実在するスクリプト（この repo の `scripts/*.sh`）を検疫したとき、Allow / Ask / Deny が何件になるか。動的な source・`$HOME`・引数の多用によって Ask が多数になる予想（§4.3）を、数字で確かめる。数字には測定コマンド・rev・日付を併記する。
- M-2 **[裁定済み Q3]** P0-a（S4）を入れたことによる Ask の増加を replay で測る。数字には測定コマンド・rev・日付を併記する。
- M-3 §9.2 の実測（2026-11-18、Claude Code 2.1.284、macOS、origin/main `dc047ef9f`）。
  - 手順: PreToolUse（matcher `Bash`）hook を 1 つずつ持つ使い捨ての plugin `pa`・`pb` を作った。hook は 2 秒待ってから、開始・終了時刻をログに書き、設定した判定を返す。これを `claude -p --model haiku --setting-sources project --plugin-dir pa --plugin-dir pb --output-format json "…touch <dir>/m_orig…"` で 1 ケース 1 回ずつ走らせ、どの marker ファイルができたかと `permission_denials` を見た。user 設定（稼働中の plugin）は読ませていない。
  - c0 allow+無判定 → 実行された。c1 無判定+無判定 → 権限確認になり、headless なので拒否（対照）。c2 allow+deny、c3 deny+allow → どちらも拒否。c4 allow+ask → 拒否（理由に「pb says ask」）。どのケースでも 2 つの hook の開始時刻の差は 0.01 秒以内で、2 秒の待ちが重なっていた（並列）。
  - c5 書き換え+無判定 → 書き換え後の `m_rewA` だけができた。c6 書き換え A+書き換え B → `m_rewB` だけができた。c7 書き換え+deny → 拒否、marker なし。
  - 測っていないこと: 対話モードでの ask の扱い、marketplace から入れた plugin（`--plugin-dir` ではない）での合成、c6 で効く側の規則（1 回だけの観測）。Bash ツールの shell がログインシェルに従うかどうか。

## 11. 実装の順序（まとめ、**[提案]**）

S3 完了 → M1（共有 crate `effectscope` への純粋移動）→ S4（P0-a を含む。Ask の増加は replay で測る）→ 検疫 plugin の sh/bash 版（parked で配布）→ canary で有効化（同時に P0-b = `.`/`source` の Ask 化）→ S5（Python）→ 検疫 plugin の Python 対応。

Q1〜Q20 と Q4-path は 2026-11-17 に裁定済み（Q4-path、Q7、Q9、Q12〈改訂〉、Q14、Q15 は第 2 次裁定）。裁定の反映で生じた §9.1 の D-1〜D-3 も 2026-11-17 に裁定済み（第 3 次裁定）。第 3 次裁定の反映で足さなかった E-1〜E-4 も 2026-11-17 に裁定済み（第 4 次裁定）。E-1 / E-4 の反映で出た食い違い 3 点（F-1〜F-3）も 2026-11-17 に裁定済み（第 5 次裁定）。
§9.2 の実測 3 件（Q21、Q22、Bash ツールが使う shell）は 2026-11-18 に測った（M-3）。未裁定は §9.1 に残した Q12 の上げ忘れ検出ゲートの置き場所と判定方法（実装の段階で改めてユーザーに聞く）である。F-1・F-2 の反映で決まらなかった 2 点（G-1、G-2）は harness-5a が判断した（§9.1）。
