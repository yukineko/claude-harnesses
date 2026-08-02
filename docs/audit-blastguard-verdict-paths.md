# blastguard — per-gate verdict-path audit

測定日 2026-08-02 / 測定点 `5b47d3ff`（監査対象は `812d0d95` 以前の 0.2.37）/ blastguard 0.2.37 → 0.2.39

> **この監査は DoD9 の分子を動かさない。** 完了条件は「未監査の verdict 経路 0本」であり、
> 本監査の到達点は **196 production サイトのうち機械列挙は完了、分類は部分的**である。
> どこまでが分類済みでどこからが未分類かは §5 に列挙した。**部分的な監査を「監査済み」と
> 数えないことがこの節の要点**であり、charter の分子は 3 of 22 のまま据え置く。

## 1. なぜ blastguard から取ったか

CLAUDE.md 第3節が三値の正典例として `crates/blastguard/src/model.rs:5`「Three answers, not two.」を
名指しし、**「二値型そのものが原因」**という主張の根拠にしている。その当の実装が一度も
全経路を列挙されていなかった。

結果として見つかったものは、この選定理由をそのまま裏づけている — **三値は ANALYSIS には
徹底されていたが、ENTRY と RECURSION には適用されていなかった**。

## 2. 方法 — 分母は機械が出す

読んで数えた「全部見た」は観測ではなく予測である（CLAUDE.md 第2節）。**自分の網羅率は
自分では観測できない**ので、分母は script が出し、**分類だけを人間（LLM）が行う**。

```
python3 scripts/census-verdict-terminals.py blastguard
python3 scripts/census-verdict-terminals.py blastguard --json
```

`scripts/census-verdict-terminals.py` は verdict を **産出する**サイトと、fallible な値を
verdict へ **潰す**サイトを列挙する。意図的に**過剰収集**する — 表に1行載って
「これは verdict 経路ではない」と分類されるコストは1行だが、載らないサイトは不可視になる。
`src/` が読めないときは exit 2（**空の census は「きれいな census」ではない**）。

測定値（測定点 `5b47d3ff`）:

| file | production サイト | 内訳の要点 |
|---|---|---|
| `detect.rs` | 166 | allow 46 / deny 43 / ask 22 / catchall\_arm 19 / empty\_collection 19 / none\_arm 15 / unwrap\_or 13 |
| `main.rs` | 6 | ask 2 / deny 2 / none\_arm 2 / catchall\_arm 1 / err\_arm 1 |
| `model.rs` | 6 | ask 4 / deny 4 |
| `exclude.rs` | 5 | catchall\_arm 1 / empty\_collection 2 / none\_arm 1 / unwrap\_or\_else 1 |
| `diffrisk.rs` | 5 | determination 3 / err\_arm 2 |
| `hookio.rs` | 3 | allow 1 / deny 1 / ask 1 |
| `interactive.rs` | 3 | ok\_erase 3 |
| `callgraph.rs` | 2 | empty\_collection 1 / unwrap\_or\_default 1 |
| **計** | **196**（テスト側 210） | permissive-or-collapsing terminal: **約 120** |

## 3. 確定した欠陥（2件、いずれも修正済み）

### 3.1 ENTRY 境界が二値だった（0.2.38、commit `812d0d95`）

ANALYSIS 側は ~25 の sub-analyser が解析不能な構文に `Ask` を返す。**玄関は二値だった。**
同一クラスの 5 サイト — *ツールは blastguard の管轄下にあり、その operand が読めなかったのに
`Allow` と記録していた*:

| site | before | after |
|---|---|---|
| `detect.rs` `detect()` Bash arm | `None => Decision::Allow` | `unreadable_operand("Bash", "tool_input.command")` |
| `detect.rs` `detect_write` | `None => return Decision::Allow` | `unreadable_operand("Write", "file_path")` |
| `detect.rs` `detect_edit` | `None => return Decision::Allow` | `unreadable_operand("Edit", "file_path/notebook_path")` |
| `main.rs` `run()` stdin parse | `None => return`（無音 ＝ allow） | 空 stdin は無音のまま／非空・解析不能は `Ask` |
| `main.rs` telemetry 順序 | `record_violation` → `println!` | `println!` → `catch_and_log(record_violation)` |

**意図的に変えなかったもの**（permissive が正しい側）:

- `_ => Decision::Allow`（**未マッチのツール**）。管轄外であり、畳み込むと Read / Grep のたびに
  prompt が出る。コメントを付けて維持。
- **空 stdin** は無音の allow のまま。「判定対象が無いと確定できた」側。

telemetry の順序は **LATENT であって live ではない**。`rule_id` / `store::now` / `cwd_or_current` /
`build_event` / `normalize_signature` を追跡して到達可能な panic は見つからなかったので、
そう記録した（誇張しない — CLAUDE.md 第6節）。

**テストが fail-open を仕様として固定していた**: `missing_or_unknown_input_is_allowed` が
`detect("Bash", None) == Allow` と `detect("Write", Some(&json!({}))) == Allow` を assert していた。
CLAUDE.md 第2節が引く `assert!(checks_verdict(&[]))` と同型。未マッチツールの半分だけを
`an_unmatched_tool_is_allowed` として残し、fail-open の2件には**逆向きの assert** を与えた。

### 3.2 recursion が nested Ask を捨てていた（0.2.39、commit `5b47d3ff`）

`unknown_wrapper_ask` は未知の verb の後ろの tail を再解析するが、**Ask を上げるのは tail が
DENY のときだけ**だった:

```rust
if detect_bash(&tail, depth + 1).is_deny() {
```

一段下では「未知の verb ＋ 破壊的な行」は **Ask** であって Deny ではない。したがって条件は偽になり、
関数末尾の `Decision::Allow` へ落ちる。**測定値（0.2.38）**:

```
dlx rm -rf /                   -> ask
pnpm dlx rm -rf /              -> ALLOW
cargo run rm -rf /             -> ALLOW
docker run rm -rf /            -> ALLOW
myrunner myrunner rm -rf /     -> ALLOW
a b rm -rf /                   -> ALLOW
a b c rm -rf /                 -> ALLOW
```

**未知の verb が2つあると1つより安全になっていた** — 認識されないトークンを1つ前置するだけで
このルールは無効化できた。

旧コードはこれを**明示的に正当化していた**:

```rust
// An Ask from the tail is not propagated: it would already have been
// reported by whichever construct produced it if that construct were
// reachable
```

到達可能性についての主張であり、**偽**である。報告する構文は「一段下のこの関数自身」であり、
その答えをここで捨てていた。CLAUDE.md 第3節が名指しする「安全な degrade だ」というコメントが、
それが免罪している当の行に付いていた形。

**スコープは意図的に限定**した。全 nested Ask を forward する版を先に試し、**測定された
benign 2件を過剰ブロック**した:

- `echo $(date 2>&1)` → Ask（tail 位置の `$(date` を command word と読む expansion Ask）
- `echo hi` を quote-only な `sh -c` 7層に包んだもの → Ask（depth-exhaustion Ask。
  `tests/backslash_escape_nesting_fail_open.rs` が ALLOW として pin しており、その理由は
  「過剰エスケープされた**形**でブロックするのは破壊的な語で判定していないから」）

したがって forward するのは unknown-verb Ask **のみ**。**残余は §5 に明記**し、
`residual_nested_ask_classes_still_collapse_to_allow` が assert する（記述だけの残余は
「きれいだと仮定されたもの」へ腐る）。

**コストは測定した**。置き換えた側のテストが「fan-out が D4 で recursion を指数にした」と
正当化していたため。`detect_bash` の呼び出しは**フレームあたり1回のまま**で、変えたのは
既に計算済みの結果の扱いだけ:

```
depth   2       90 us       depth  32      621 us
depth   8      248 us       depth 128     3666 us
depth  16      354 us       depth 256     7349 us
```

線形。D4 の論拠はこの変更には当たらない。

## 4. 棄却した候補（記録する — CLAUDE.md 第6節）

**棄却にも発見と同じ立証責任を課す。** 記録しないと次の読み手が再提起する。
いずれも `crates/blastguard/tests/verdict_path_probes.rs` にテストとして残した。

| 候補 | 読みでの予測 | 測定結果 |
|---|---|---|
| multi-candidate exec wrapper が `unknown_wrapper_ask` の単一候補ゲートから落ちる | fail-open | **REFUTED** — `sudo`/`env`/`nohup`/`timeout`/`nice`/`stdbuf`/`command` ＋ `rm -rf /` は全て deny。認識済み wrapper は先に `analyze_command_at` が処理する |
| `analyze_xargs` の `xargs_command_start → None` が内側のコマンドを隠す | fail-open | **REFUTED** — `-I{}` `-I {}` `-J %` `--replace={}` `-P4 -n1` 末尾 `--` を含む 11 綴りすべて内側に到達 |
| `chmod`/`chown` の mirror gap | 未確定 | **RECORDED, not asserted** — `chmod 000 .githooks/pre-commit` は deny、`chown nobody` と `chown :staff` は Allow。chown が何かを disarm しうるかは **OS についての問い**であって本 crate の問いではないので、根拠なく Deny を assert しない |

## 5. 既知の permissive 集合（未解決 — 「clean」ではない）

- **`unknown_wrapper_ask` の残る nested Ask クラス**（depth-exhaustion / expansion）は
  今も `Allow` へ潰れる。0.2.39 の regression ではない（旧 `.is_deny()` も同様に捨てていた）が、
  **clean ではない**。閉じるには expansion Ask が command 位置と argument 位置を区別する必要がある。
- **`crates/blastguard/src/classify.rs:216`** —
  `if detect::detect("Bash", Some(&json!({ "command": text }))).is_deny() {`
  のみで risk を上げる。**`Ask` は
  `Risk::Low` + `reversible: true` へ落ちる**。消費側（condukt の gate/policy）ではこれが
  AutoExec 条件そのものなので、「blastguard が解析できなかったコマンド」が人間承認なしで
  実行されうる。**backlog `ed941047`**（policy 判断を含むため本監査では変更しない）。
- **`detect.rs` の未分類サイト** — census が挙げた約 120 の permissive-or-collapsing terminal の
  うち、個別に逐語引用つきで分類したのは `Decision` 産出サイトと本文書が挙げたものに留まる。
  残りは主に parser helper の accumulator 初期化（`let mut out = Vec::new()`）と
  sub-predicate の `_ => false` であり、**カテゴリとしては verdict 経路ではないと判断したが、
  個別には検証していない**。これが §冒頭で「分子を動かさない」と書いた理由である。
- **`crates/blastguard/src/detect.rs:527`**
  `fn sort_output_file<'a>(rest: &[&'a str]) -> Option<&'a str> {`
  — `-o` / `--output` が値なしで行末に来ると
  `rest.get(i + 1)` が `None` を返し、`analyze_sort` の `None => Decision::Allow` に落ちる。
  「operand が欠けている」を「output が無い」に写している。**実害は無い**（値なしの `-o` は
  sort 自身がエラーで終了し、何も書かない）ので **NON-EXPLOITABLE な conflation として記録**し、
  live な fail-open とは呼ばない。

## 6. 使えるようになった一般化

`scripts/census-verdict-terminals.py` は crate 名を取る。DoD9 の残り 18 crate に同じ分母を
機械的に出せる。**分類は依然として人間側の仕事**であり、そこが監査の実体である。
