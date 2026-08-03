# mutategate — per-gate verdict-path audit

> 監査対象 **mutategate 0.1.6**、測定点 `171062fc`、測定日 2026-08-04。
> 分母は `scripts/census-verdict-terminals.py mutategate` が返す production の
> verdict-terminal サイト **10 件**（tests 0 件）。本文書は **10 件すべてを個別に**
> 分類する。カテゴリ単位の棄却は行わない（CLAUDE.md 第6節。blastguard 第1ラウンドが
> 「残りは parser helper 中心」とまとめた結果 `exclude.rs` の実在する fail-open を
> 隠した実例があるため、この監査では「残余」という語を使わない）。
>
> 本監査は compass charter の **DoD9** の分子を 3 → 4 へ動かす（schemaguard /
> autoflow / budgetguard に続く 4 本目）。GATE crate としては **最初の1本**。

## 0. 要旨

**mutategate の verdict 経路は、本体（block/allow を決める経路）については全件が
restrictive 側に解決していた。** 10 件中 9 件は fail-closed か display-only で、
うち 3 件は既に自分の docstring で理由を明文化している（CA-mutategate-01 / 02）。

**1 件だけ、carve-out の条件を満たしていないのに carve-out を使っている経路がある** —
emit_violation（§3 の F-1 / F-2） の `emit_violation`。これは「判定を持たない fail-soft な
telemetry」を自称しているが、**下流の消費者が存在し、欠落が unknown として表示されない**
ため、CLAUDE.md 第1節が carve-out を許す条件（「消費者が存在しないか、欠落が UI 上で
明示的に unknown と表示される場合だけ」）を満たしていない。§3 で扱う。

## 1. 分母（census 出力の逐語）

```
=== mutategate: 10 production verdict-terminal sites (0 in tests) ===

lib.rs             3 sites  {"catchall_arm": 1, "empty_collection": 1, "none_arm": 1}

main.rs            7 sites  {"determination": 3, "err_arm": 2, "none_arm": 1, "unwrap_or_else": 1}
```

census が「要分類（permissive-or-collapsing）」として挙げたのは 7 件。残る 3 件は
`Determination` の 3 アーム（`crates/mutategate/src/main.rs:65`/`66`/`74`）で、これは既に三値を受けている側なので
census が permissive 候補から外している。ただし**「三値を受けている」と「三値を restrictive に
解決している」は別**なので、本監査では 3 件も個別に確認する（§2 の A-1〜A-3）。

## 2. 全 10 サイトの分類

### A. 入力境界（`Determination` を受ける 3 アーム）

| # | 位置 | 逐語引用 | 解決先 | 判定 |
|---|---|---|---|---|
| A-1 | `crates/mutategate/src/main.rs:65` | `Determination::Known(Some(j)) => j,` | 読めた本文 | 正常系 |
| A-2 | `crates/mutategate/src/main.rs:66` | `Determination::Known(None) => {` … `return ExitCode::from(2);` | **exit 2** | **restrictive** |
| A-3 | `crates/mutategate/src/main.rs:74` | `Determination::Undetermined(why) => {` … `return ExitCode::from(2);` | **exit 2** | **restrictive** |

「ファイルが無い」(A-2) と「読めなかった」(A-3) を**別々のアームで別々のメッセージ**にし、
どちらも exit 2（＝評価不能）へ落としている。**両者を一つに畳んでいない**点が重要で、
`Determination` を受けておきながら `Known(None)` と `Undetermined` を同じ扱いにする実装は
三値を受けた意味を失うが、ここはそうなっていない。exit code 表（`main.rs:13-16`）も
`2` を「usage/IO/parse error (could not evaluate the gate at all)」と定義しており、
**判定不能を pass(0) でも fail(1) でもない第三の出口へ出している**。

### B. パース・集計

| # | 位置 | 逐語引用 | 解決先 | 判定 |
|---|---|---|---|---|
| B-1 | `crates/mutategate/src/main.rs:86` | `Err(e) => {` … `return ExitCode::from(2);` | **exit 2** | **restrictive** |
| B-2 | `crates/mutategate/src/lib.rs:190` | `_ => s.unknown += 1,` | `unknown` へ計上 | **restrictive** |

B-2 は catchall アームだが、**捨てていない**。`unknown` は
`MutationSummary::viable()` の分母に入り killed には入らないので、未知の状態が
kill-rate を**押し上げることはできない**。自分の docstring がその理由を述べている
（`lib.rs:171-175`）:

> Unknown/absent `summary` values are **tracked, not dropped**: they land in
> `unknown`, which counts toward the viable denominator but not toward killed, so a
> `cargo-mutants` state this crate doesn't recognise (e.g. a future new state, or a
> malformed record) can never silently inflate the apparent kill-rate (CA-mutategate-01).

さらに `evaluate` が `unknown > 0` のとき reason 文字列に警告を差し込む
（`lib.rs:265-270`）ので、**黙って restrictive にするのではなく、restrictive にしたことを
表示している**。これは第1節の「判定しないものは、失敗しても判定したふりをしない」の
正しい満たし方。

### C. 判定本体

| # | 位置 | 逐語引用 | 解決先 | 判定 |
|---|---|---|---|---|
| C-1 | `crates/mutategate/src/lib.rs:231` | 「harness_core::verdict::Verdict::from_findings(Vec::new())」 | `Clean` | **正当** |
| C-2 | `crates/mutategate/src/lib.rs:274` | `None => GateOutcome {` … `passed: false,` | **fail** | **restrictive** |

**C-1 は census が `empty_collection` として挙げたが、fail-open ではない。**
CLAUDE.md 第3節が禁じる空集合は「エラー時に空を返し、下流が『検査対象なし＝合格』と読む」
ものだが、ここの空 `Vec` は**エラー経路ではなく、`self.passed` が真である
（＝実測した kill-rate が閾値以上だった）と確定した後の分岐**にある:

```rust
if self.kill_rate.is_none() {
    harness_core::verdict::Verdict::undetermined(self.reason.clone())
} else if self.passed {
    harness_core::verdict::Verdict::from_findings(Vec::new())
} else {
    harness_core::verdict::Verdict::violation(self.reason.clone())
}
```

**三値の順序が正しい**: 「測れなかった」(`kill_rate.is_none()`) を**最初に**捌き、
Clean へ落ちうる経路から除外している。`Verdict` の `Clean` は private witness でしか
作れないので、この空 `Vec` は「findings を実際に集めた結果ゼロだった」という
**証拠の提示**であって、判定の省略ではない。

C-2 は「viable mutant がゼロ ＝ 何も測れていない」を `passed: false` にしている。
測れていないものを pass にしないという、第3節そのままの解決。

### D. exit code の合流

| # | 位置 | 逐語引用 | 判定 |
|---|---|---|---|
| D-1 | `crates/mutategate/src/main.rs:106` | `None => println!(` `"  kill-rate: n/a       threshold: {:.1}%",` | display-only |

`kill_rate` が無いときに `n/a` と**明示表示**する。空欄・0% ではない。第1節が
statusline の空表示を fail-open と認定した（`3b1eb24`）のと同じ論点で、ここは
`unknown` に相当する語を出しているので正しい側。

判定の合流点そのものは census の permissive 候補に入っていないが、監査の完全性のため
記録する（`main.rs:115-124`）:

```rust
let verdict = outcome.verdict();
if verdict.blocks() {
    eprintln!("  FAIL: {}", outcome.reason);
    emit_violation(&outcome, &cli.outcomes);
    ExitCode::from(verdict.exit_code(1) as u8)
```

**private な `passed` bool ではなく共有三値の `Verdict::blocks()` で分岐している。**
`Undetermined` と `Violation` は `blocks()` が両方 true なので、**判定不能は
block 側へ合流する**。これが DoD9 が各 crate に求めている形そのもの。

### E. 閾値の sanitize

census の permissive 候補には出ないが、第3節が名指ししている「床なし clamp」に
該当しうるので確認した。main.rs の該当分岐 が `validate_min_kill_rate` の `Err` を
exit 2 にしており、`Cli` の docstring（`main.rs:49-53`）が契約を述べている:

> Must be in the half-open range `(KILL_RATE_EPSILON, 1.0]`: any value `<= 1e-9`
> (`0.0`, negative, or sub-epsilon) is REJECTED because it would disable the gate —
> a 0% kill-rate is bridged to a pass by the epsilon tolerance, so such a floor
> always passes.

**clamp ではなく reject** である。第3節の「閾値の sanitize はゲートを無効化しない範囲に
clamp する」を、clamp せず拒否することで満たしている（`cde2212c` の floorless-clamp 修正の
結果。charter が言うとおりこれは点の修正であって監査ではないが、監査した結果として
現在は正しいことをここで確認した）。

## 3. 唯一の finding — `emit_violation` の carve-out が条件を満たしていない

| # | 位置 | 逐語引用 | 解決先 | 判定 |
|---|---|---|---|---| <!-- doc-claim-exempt: historical quote — the PRE-FIX text of emit_violation, measured at mutategate 0.1.6 (commit 171062fc). The same commit that adds this audit replaces those lines, so the quotes intentionally no longer resolve; they are the record of the state that was fixed. The post-fix code is quoted in §3. -->
| F-1 | `crates/mutategate/src/main.rs:166` | `Err(_) => return,` | **無言 return** | **finding** | <!-- doc-claim-exempt: historical quote — the PRE-FIX text of emit_violation, measured at mutategate 0.1.6 (commit 171062fc). The same commit that adds this audit replaces those lines, so the quotes intentionally no longer resolve; they are the record of the state that was fixed. The post-fix code is quoted in §3. -->
| F-2 | `crates/mutategate/src/main.rs:191` | `let _ = overwatch::store::append_violation(&cwd, &event);` | **無言 破棄** | **finding** | <!-- doc-claim-exempt: historical quote — the PRE-FIX text of emit_violation, measured at mutategate 0.1.6 (commit 171062fc). The same commit that adds this audit replaces those lines, so the quotes intentionally no longer resolve; they are the record of the state that was fixed. The post-fix code is quoted in §3. -->
| F-3 | `crates/mutategate/src/main.rs:176` | `.unwrap_or_else(\|_\| format!("pid-{}", std::process::id()));` | pid 代替 | 正当 |

F-3 は無害。`CLAUDE_CODE_SESSION_ID` が無いときに pid で代替するのは、識別子を**失う**のでは
なく**別の実在する識別子に落とす**動作で、記録は残る。

**F-1 と F-2 が finding。** 関数の docstring はこう自称している（`main.rs:160-163`）:

> Record a fleet-level violation for a FAILed gate, fail-soft: never changes the
> gate's exit code or stdout/stderr, and never panics when the overwatch store is
> unwritable (e.g. sandboxed/read-only HOME, missing repo root).

そして F-2 の直前のコメントはこう書く:

> // Best-effort: any store I/O failure is swallowed, not surfaced.

**「exit code を変えない」という設計判断そのものは正しい。** ゲートは既に FAIL を
出しており（`emit_violation` は `verdict.blocks()` の中でしか呼ばれない）、
telemetry の失敗でゲートの判定を変えるべきではない。**問題は exit code ではなく、
失敗が完全に不可視であること。**

CLAUDE.md 第1節は carve-out の条件を明示している:

> **「判定を持つ」は返り値の型ではなく消費のされ方で決まる** — 下流（人間・スクリプト）が
> 沈黙・空・既定値を「問題なし」と読める出力は、型に関わらず判定を持つ。（…）
> 判定を持たないのは、消費者が存在しないか、欠落が UI 上で明示的に unknown と
> 表示される場合だけ。**この分類は自己申告で免責を得る道具になりうる**ので、
> 免責を主張するモジュールは下流消費者を列挙すること。

`emit_violation` は免責を主張しているが、**下流消費者を列挙していない**。実際には存在する:

```
mutategate emit_violation
  → overwatch::store::append_violation  (violation ledger)
    → overwatch::store::scan_violations
      → violation::detect_recurrence(..., RecurrencePolicy::default())
        → .filter(|r| r.is_systemic)
          → review_queue::build_queue → bridge::run_in → backlog add (p0)
```

（経路は `crates/overwatch/src/bridge.rs:267-281` で確認。systemic violation は
`severity_to_priority` の high 相当として **p0** で backlog に入る。）

したがって欠落の意味はこうなる。**再発回数が閾値に届かず、systemic として検出されない。**
読み手側（`scan_violations`）は既に三値化されており、`Undetermined` を
「systemic violation ゼロ」と報告しないよう作られている — `crates/overwatch/src/bridge.rs:275` の警告文が
その意図を述べている:

> "overwatch --to-backlog: WARNING — the violation ledger could not be read
>  or held an undecodable line; NO systemic-violation entries were bridged
>  from it. This is NOT a report of zero systemic violations; re-run once
>  the store is readable."

**読み側がここまで厳密に「読めなかった ≠ ゼロ」を守っているのに、書き側が
黙って書かないと、読み側は正常に読める空の台帳を見て「ゼロ」と正しく報告する。**
読み側の三値化は、書き込み自体が起きなかったケースを救えない。これが本 finding の要点で、
第6節の言う **ミラーギャップ**（読みだけ直して書きが取り残される）の一形態である。

### 修正の方向（exit code は変えない）

- **exit code / stdout は不変**（fail-soft 契約は正しいので維持する）。
- **失敗を stderr で可視化する**。gate は既に `eprintln!("  FAIL: …")` を出しているので、
  そこへ 1 行足すだけで「記録できなかった」が人間に見える。
- 「記録した」と「記録できなかった」を**呼び出し側が区別できる型**にする（現状は `()`）。

つまり第1節の carve-out を**満たすように直す**（欠落を unknown として表示する）のであって、
telemetry をゲートに昇格させるのではない。

## 4. 集計

| 分類 | 件数 |
|---|---|
| restrictive（fail-closed / 判定不能を block 側へ） | 6（A-2, A-3, B-1, B-2, C-2, および D の合流点） |
| 正当な Clean / display-only / 無害 | 3（C-1, D-1, F-3） |
| 正常系アーム | 1（A-1） |
| **finding** | **2（F-1, F-2 — 同一原因の 1 件）** |

**未分類 0 件。** census が返した 10 件と、census が permissive 候補から外していた
`Determination` の 3 アーム、および census の対象外だった閾値 sanitize（§2 E）と
exit code 合流点（§2 D）まで含めて個別に確認した。

## 5. この監査が証明していないこと

- **`overwatch::store::append_violation` の内部**は監査していない。本監査の対象は
  mutategate の verdict 経路であり、overwatch 側は別 crate の per-gate 監査に属する
  （DoD9 の分母 22 に overwatch は別途含まれる）。
- **`cargo mutants` が生成する `outcomes.json` の正しさ**は対象外。本 crate は
  「与えられた JSON をどう判定に写すか」だけを担う。
- F-1 の `std::env::current_dir()` が失敗する状況を**実際に再現してはいない**
  （cwd が削除された場合等）。F-2 の方は再現・観測した（§ 修正コミット参照）。
  F-1 は F-2 と同一の修正で同時に可視化されるため、別個の再現は要求しない。
