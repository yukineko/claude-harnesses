<!-- record: 2026-10-01 rev: 74aad11fea9e5ee7aae2086ca37fd96beddfcacf -->
# Determination 直接 match 監査 (backlog e269274f)

`harness_core::verdict::Determination<T>` は variant が pub のため、`require()` を経由せず直接 `match` / `if let` / `matches!` できる。`require()` 経由(約45箇所)は監査済みだが、直接 match 側(2026-09-29 のトリアージで `Determination::Undetermined` 腕が harness-core 外に281件)は全体として未監査だった。本書はその全件列挙と、各 Undetermined 腕がどちら側に解決するかの分類である。

> 本書は**コードを一切変更しない**。PERMISSIVE / UNCLEAR は修正候補であり、修正は別タスクで行う。
> 監査者は対象コードの実装者ではない(独立監査)。分類は各腕と消費者のコードを読んで行った。読めなかったものは UNCLEAR とし、RESTRICTIVE に推測で倒していない(CLAUDE.md §6)。

## 1. 測定条件

- 測定点 rev: `74aad11f` (branch `flow-34385c9e/det-audit`、main と同一コミットから分岐)
- 測定日: 2026-10-01
- 対象: `crates/` 以下の `*.rs` のうち `crates/harness-core/` を**除く**全ファイル(`crates/*/target` は走査対象外)

### 1.1 機械的な列挙(grep)

```
G() { cd <worktree> && grep -rnE "$1" crates --include='*.rs' | grep -v '^crates/harness-core/'; }

G '\bDetermination::(Undetermined|Known)\b' | wc -l      # => 1041 (構築と pattern の両方、test・コメント含む)
G 'Determination::Undetermined'               | wc -l      # => 402
G 'Verdict::Undetermined'                      | wc -l      # => 40  (ChecksVerdict:: 等の局所 enum も部分一致で含む)
G 'Required::Blocked'                           | wc -l      # => 68  (require() の destructure。既監査領域、本書の集計対象外)
G 'Determination::\{|Verdict::\{|Determination::\*|Verdict::\*'   # => 0 件(glob/グループ import で名前を隠した直接 match は無い)
grep -rl '\bDetermination\b' crates --include='*.rs' (harness-core 除く) | wc -l   # => 118 ファイル
```

`Undet` payload は harness-core 内でしか mint できない(`Determination::Undetermined(Undet)`)ため、harness-core 外に現れる `Determination::Undetermined` は**全て pattern 位置か doc/コメント**であり、構築(`Determination::undetermined(..)` 小文字)とは区別できる。

### 1.2 prod / test / コメントの仕分け

grep の生ヒット 402+40 行には局所 enum(`ChecksVerdict::Undetermined` 等)・コメント・テストが混ざる。単語境界付きの正規表現 `(?<![A-Za-z_])(Determination|Verdict)::Undetermined\b` で再走査し、`#[cfg(test)]` 項目(波括弧対応で範囲を取得)・`tests/` 配下・`tests/ui/` をテスト、`//` `*` 行をコメントとして除外した。

| 区分 | Determination:: | Verdict:: |
|---|---:|---:|
| コメント/doc | 34 | 7 |
| テスト(cfg(test) / tests/ / ui fixture) | 112 | 7 |
| **prod(本書の分類対象)** | **253** | **13** |

prod 266 件から、波括弧対応が外れた/テスト専用であることを**個別に確認して**さらに 9 件を除外した: `condukt/src/maintree.rs` の `mod tests` 内 6 件(1216,1257,1284,1298,1345,1364)、(mod tests 内)、`stuckguard/src/verdict_monotonicity.rs` の 2 件( が `#[cfg(test)] mod verdict_monotonicity;` で、ファイル全体がテスト専用。『THE DEFECT』を意図的に再現する mutation 用フィクスチャ)。

**分類対象は prod の Undetermined 腕 257 件(Determination 245 / Verdict 12)**。
トリアージの『281』は test・doc を含む粗い数で、prod に絞ると 257 である(数字は継承せず測り直した)。

### 1.3 Undetermined を名指ししない直接 match

`Determination::Known(..)` だけを書き、Undetermined を wildcard / else / `matches!` に落とす形は grep `Undetermined` では拾えない。prod の `Determination::Known` pattern 行のうち、前後 25 行に Undetermined 腕が無いものを別途抽出した(§3.2)。

## 2. 集計

| 区分 | 件数 | 意味 |
|---|---:|---|
| PERMISSIVE | 10 | 空・既定値・skip・allow に写す(または下流が沈黙を OK と読み得る) |
| UNCLEAR | 6 | 最終消費者まで辿れなかった |

- FORWARDED の最終消費者は各行に書いた。消費者側が制限側であることを確認できたものは理由欄にそう書いてある。書いていないものは『転送』までしか確認していない。

## 3. 修正候補(PERMISSIVE / UNCLEAR)

### 3.1 PERMISSIVE(10 件)

重い順。★は下流が沈黙/既定値を OK と読み得る、または判定系の近くにあるもの。

| # | file:line | 理由と消費者 |
|---:|---|---|
| 1 | crates/blastguard/src/reversible.rs:286 | git status 判定不能 → GitState::Undetermined を作るが、decide_recovery の Repo 腕が `let _ = git;` で**読まずに捨て**RecoverableFromGit を返す。消費:  が ctx.confined_root が Some なら無人実行を許可。2026-09-18 の operator ruling(doc に明記)。消去は callsite(decide_recovery)側 |
| 2 | crates/blastguard/src/reversible.rs:288 | git status 判定不能 → GitState::Undetermined を作るが、decide_recovery の Repo 腕が `let _ = git;` で**読まずに捨て**RecoverableFromGit を返す。消費:  が ctx.confined_root が Some なら無人実行を許可。2026-09-18 の operator ruling(doc に明記)。消去は callsite(decide_recovery)側 |
| 3 | crates/autoflow/src/lock.rs:58 | find_backlog_binary 判定不能 → `return true`(driver active = 'stand down')。 が `return` し autoflow の Stop block 自体を行わない = この Stop は素通り。doc は restrictive と自称するが、Stop ゲートから見れば skip |
| 4 | crates/propguard/src/derive.rs:185 | criteria_file 読めず → stderr 警告のうえ **inline done_criteria があればそれで続行**。ゲートは走るが、意図した基準(file)と異なる基準で評価される(既定値への fallback)。inline も空なら Undetermined を転送(205) |
| 5 | crates/playbook/src/main.rs:148 | UserPromptSubmit hook: 読めない store を『空と同じ』= 注入なしで**無言 return**(コメントが 'treated the same as an empty one' と自認)。stderr 出力なし。消費者は model のコンテキスト(注入ノートの欠落を知り得ない) |
| 6 | crates/runbook/src/main.rs:122 | playbook と同型: 読めない store を空と同じ扱いで無言 return(runbook 注入が黙って欠落) |
| 7 | crates/overwatch/src/store.rs:376 | read_jsonl_best_effort: `Known(None) \| Undetermined(_) => Vec::new()`。doc が自認する『判定不能 → 空』。pub wrapper 10 本(read_events/rollbacks/review_findings/bridged_*/dispositions/runtime_conflicts/review_findings_all/merge_conflicts/merge_conflict_resolutions)が消費。production の消費者を追跡した結果(specguard ,  compact, 1844-1845, 1899, condukt )はいずれも『空 = 保守側』に倒れる形だったが、wrapper は pub のまま残る(latent) |
| 8 | crates/condukt/src/circuit.rs:506 | journal の `CircuitRecord::idle_secs: i64` に判定不能を **0** で記録(コメントが 'KNOWN RESIDUAL' と自認)。stdout JSON は null だが永続 journal は 0。消費: load_circuit_records はテストからのみ(production 消費者は grep で未発見)。idle 軸 off 時は 0 が測定値と区別できない |
| 9 | crates/session-insights/src/main.rs:203 | subagent 読み不能 → stderr のみで under-count の turns を record に書き続ける( → record::write_from_session)。ノート本文の turns 数値に不完全の注記は無い(cost 側は inline 開示=)。消費者は Obsidian ノート。low |
| 10 | crates/specguard/src/scope.rs:506 | relevant_file_map: code index 判定不能 → `Vec::new()`(base set のみ)。doc が『additive & advisory, 下流が短い map を clean と読まない』と主張。consumer を追跡:  の prompt 描画(auditor への読み順ヒント)のみで verdict 計算は見つからず。無音。low |

#### 逐語引用と消費者の追跡

**P1 crates/blastguard/src/reversible.rs:286,288 — 消去は callsite ではなく `decide_recovery`(:174)にある**

```rust
//   (probe)
Determination::Known(out) => match out.stdout_allowing(&[0]) {
    Determination::Known(stdout) => decide_git_state(true, true, &stdout),
    Determination::Undetermined(_) => decide_git_state(true, false, ""),   // -> GitState::Undetermined
},
Determination::Undetermined(_) => decide_git_state(false, false, ""),     // -> GitState::Undetermined

//   (decide_recovery)
let _ = git;
...
RepoProbe::Repo => Recovery::RecoverableFromGit,
```

`git status` が答えなかった場合でも、`RepoProbe::Repo`(work tree 内)と判定済みなら `RecoverableFromGit` になる。消費者  は `RecoverableFromGit => ctx.confined_root("redirect", ..).is_some()` なら無人実行(プロンプト省略)を許す。doc(:135-160)に operator ruling 2026-09-18 として『`Repo` 行は git 状態を見ない』『この保護を手放す』と明記されている = **意図的な緩和**。ただし『git が答えなかった』ことまで捨てているかは ruling 文言(『gitで復元できるもの』)の射程外にも読めるため、人間判断に返す候補。

**P2 crates/autoflow/src/lock.rs:58**

```rust
Determination::Undetermined(_) => return true,   // backlog_driver_active: 'stand down'
```

消費者 : `if lock::backlog_driver_active(&cwd) { return; }` — `stop_run` の先頭で抜け、以降の block 判定(record-requested / condukt 状態 / backlog 残)を**全て行わない** = この Stop は素通り。docは『stand down が制限側』と書くが、それは『二重駆動の回避』という意味での制限であり、Stop ゲートとしては allow。§1 の『docstring が実挙動と違う安全な話を書く』に該当する可能性があり、記述か挙動のどちらかを直す候補。

**P3 crates/propguard/src/derive.rs:185**

```rust
Determination::Undetermined(reason) => {
    eprintln!("propguard: cannot read criteria_file {}: {}", ...);
    unreadable = Some(reason);
}
...
if !inline.is_empty() {
    if unreadable.is_some() { eprintln!("propguard: falling back to inline done_criteria"); }
    return Determination::Known(Some(inline.to_string()));
}
```

criteria_file が読めないとき、inline `done_criteria` が非空ならそれで `Known` を返す。ゲート自体は走る(allow ではない)が、意図した基準とは別の基準で評価される。通知は stderr のみ。inline も空なら Undetermined を転送し  が `criteria-unreadable` で Block する。

UserPromptSubmit 注入 hook。読めない store を『空と同じ』と自認し、**stderr すら出さず**無言で return。消費者は model のコンテキストで、注入されるはずだったノート/runbook の欠落を知り得ない。同じ crate の CLI 側は `unknown — store could not be read` と明示して exit 1 しているので、hook 側だけが沈黙。コメントの『must never fail a turn』は CLAUDE.md §1 が撤去した正当化と同型。ただし hook は verdict を返さないので、少なくとも stderr での unknown 表明が最低線。

**P6 crates/overwatch/src/store.rs:376 — `read_jsonl_best_effort`** (解消済み: backlog e8a61ec3 で `Result<Vec<T>>` 化し、読めない台帳は `Err`。以下は修正前の記録)

```rust
Determination::Known(None) | Determination::Undetermined(_) => Vec::new(),
```

doc(:366-372)が『the very collapse `scan_jsonl` exists to avoid』『nothing that makes a DECISION should call one』と自認する意図的な空集合化。pub wrapper 10 本が消費する。production 消費者を全て追跡した:

| wrapper | production 消費者 | 空になったときの向き |
|---|---|---|
| `read_events` / `read_rollbacks` / `read_runtime_conflicts` / `read_review_findings_all` / `read_bridged_entries` | production 消費者は grep で**未発見**(テストのみ) | — |

現状の消費者は全て保守側に倒れるので、現時点の実害は確認できなかった。ただし wrapper は pub のままで、新しい呼び出しが判定に使えば fail-open になる(latent)。

**P9 crates/condukt/src/circuit.rs:506**

```rust
let journaled_idle_secs = match &idle {
    Determination::Known(secs) => *secs,
    Determination::Undetermined(_) => 0,
};
```

コメント(:488-503)が『KNOWN RESIDUAL』と自認。stdout JSON は `null` だが、永続 journal(`CircuitRecord::idle_secs: i64`)には 0 が入る。`idle_ttl_secs==0`(idle 軸 off)のときは 0 が測定値と区別できない。`load_circuit_records` の消費者はテストのみで production 消費者は見つからなかった。修正は `Option<i64>` 化(コメントが follow-up と記載)。

**P10 crates/session-insights/src/main.rs:203** — sub-agent の turns を読めず under-count になっても、記録は書き続け、通知は stderr のみ。ノート本文の turns 数値に不完全の注記は無い(cost 側は  で inline 開示)。消費者は Obsidian ノート。low。

**P11 crates/specguard/src/scope.rs:506** — code index が答えなければ `Vec::new()`(base set のみ)。doc は『additive and advisory、verdict を計算しない』と主張。`relevant_file_map` の呼び出しは (prompt 描画)のみと確認。無音。low。doc 自身が『下流が内容から結論を出し始めたら Determination を返せ』と条件を書いているので、その条件のガードが無い点だけが残る。

### 3.2 UNCLEAR(6 件)

いずれも『観測用 hook が判定不能時に stderr へ書いて何もしない』形。**stderr が人間または下流に届くかを検証していない**ため、RESTRICTIVE ではなく UNCLEAR とした。
判定(verdict)を消費する下流は見つけていない。もし届かないなら playbook/runbook と同じ PERMISSIVE(沈黙)に落ちる。

| file:line | 内容 |
|---|---|
| crates/context-governor/src/backing.rs:93 | snapshot を保存せず stderr に 'なかった のではない' と明記してスキップ。ただし hook の stderr が人間/消費者に届くかは未検証、判定を消費する下流も見つからず(observability) |
| crates/ctxrot/src/hooks/distill.rs:133 | 蒸留をスキップ。stderr に 'なかった のではない' と明記してスキップ。ただし hook の stderr が人間/消費者に届くかは未検証、判定を消費する下流も見つからず(observability) |
| crates/ctxrot/src/hooks/guard.rs:547 | 再浮上なし(return None)。stderr に 'なかった のではない' と明記してスキップ。ただし hook の stderr が人間/消費者に届くかは未検証、判定を消費する下流も見つからず(observability) |
| crates/ctxrot/src/hooks/rescue.rs:78 | 退避ノートを作らない。stderr に 'なかった のではない' と明記してスキップ。ただし hook の stderr が人間/消費者に届くかは未検証、判定を消費する下流も見つからず(observability) |
| crates/ctxrot/src/hooks/restore.rs:69 | 引き継ぎを注入しない(None)。stderr に 'なかった のではない' と明記してスキップ。ただし hook の stderr が人間/消費者に届くかは未検証、判定を消費する下流も見つからず(observability) |
| crates/ctxrot/src/hooks/restore.rs:82 | 引き継ぎを注入しない(None)。stderr に 'なかった のではない' と明記してスキップ。ただし hook の stderr が人間/消費者に届くかは未検証、判定を消費する下流も見つからず(observability) |

確認方法の提案: Claude Code が hook の stderr をどの経路で表示するか(SessionStart / UserPromptSubmit / Stop 別)をテストで観測する。観測できなければ人間に ask。

### 3.3 Undetermined を名指ししない直接 match(grep では拾えない形)

これらは wildcard が制限側に倒れるよう**意図して書かれている**ことをコメントとコードで確認した。ただし『将来 variant が増えても同じ』という保証は型では無い(`Determination` は 2 variant で `#[non_exhaustive]` でもなく exhaustive match になるので、新 variant は pattern 側でもコンパイルエラーになる)。

## 4. 全件表(crate 別)

凡例: R=RESTRICTIVE / F=FORWARDED / P=PERMISSIVE / U=UNCLEAR。kind は `Determination` または `Verdict`(`Verdict::Undetermined`)。
`Required::Blocked(Verdict::Undetermined(..))` の destructure は require() 経路なので既監査領域だが、Verdict 腕として数えたうえで F にしてある。

> **追記 2026-10-02（測定点 4498c50d）**: 上の行と下の表の  の `Required::Blocked(Verdict::Undetermined(r))` は、本書の監査時点の形である。490f12c6（backlog 1a6c1c48）で `Required::Blocked` のペイロードが `Verdict` から `Undet` に狭まり、これらの腕は今 `Required::Blocked(r)`になっている。`Blocked(Clean)` は外部から構築できない（negative fixture `crates/harness-core/tests/ui/verdict/forge_blocked_clean.rs`）。判定 F（転送）は変わらない。行番号と §0 の件数は監査時点の値で、再測定していない。

## 5. この監査が主張しないこと(限界)

- 『消費者まで追った』と書いた行以外は、Undetermined 腕の**腕自身の本体**を読んだだけである。腕が `Determination::Undetermined(why)` を返している FORWARDED の最終消費者全てを辿ってはいない。
- `Option`/`Result`/`bool` に潰された後の下流(例: `session::load_one` が Option を返す側の消去)は、`Determination` の直接 match ではないので範囲外。CLAUDE.md の『消去は callsite にある』(erasure-lives-at-callsite)に従えば、別の監査軸として必要。
- テスト範囲の判定は波括弧対応によるヒューリスティックで、外れた 9 件は個別に確認して除外した。逆に『テストと判定されたが実は prod』が残る可能性は、`#[cfg(test)]` 項目の後ろに prod 項目が続くファイル(backlog/lock.rs、condukt/claim.rs 等)について関数名で個別確認し、31 候補を個別確認して 24 件を prod に戻し(7 件はテストと確認)潰したが、網羅の保証は無い。
- テスト・ui fixture に現れる `Determination::Undetermined`(112+7 件)は分類していない。`overwatch/tests/verdict_monotonicity.rs` と `stuckguard/src/verdict_monotonicity.rs` は mutation 用に『意図的な欠陥』を含む。
- 件数は `Determination::Undetermined` の出現**行**であり、1 行に複数 pattern がある場合でも 1 件と数えている。

## 6. 再現手順

1. `git -C <worktree> rev-parse --short HEAD` が `74aad11f` であることを確認
2. §1.1 の grep を実行して生カウントを確認
3. prod の Undetermined 腕の列挙: `#[cfg(test)]` 項目の範囲(波括弧対応)・`tests/`・`//` 行を除き、正規表現 `(?<![A-Za-z_])(Determination|Verdict)::Undetermined\b` を行単位で適用 => 266 行、検証済みテスト 9 件を除外して 257 行
4. 各行の前後 5 行を読み、腕の本体と消費者を分類(表 §4)

<!-- /record -->

<!-- record: 2026-10-15 rev: eb7a739aef03d352dc97e64b78133e50a8a636cb -->
## 7. 機械検証つき逐語引用

docs の check-doc-claims.py は、バッククォートで囲んだ パス:行 とその直後の引用を index と照合する。本書の他の箇所は誤検知(短縮パス・複数行指定・同一行の別引用)を避けるためバッククォートを外してあるので、主要な PERMISSIVE 箇所だけをここで機械検証可能な形で固定する。

- crates/blastguard/src/reversible.rs の decide_recovery（旧 :174「let _ = git;」/ 旧 :188「RepoProbe::Repo => Recovery::RecoverableFromGit,」）— backlog 7778b634 で是正済み（読めない git 状態は Undetermined を返す）。機械検証の対象から外した。
- `crates/autoflow/src/lock.rs:58` 「Determination::Undetermined(_) => return true,」
- `crates/overwatch/src/store.rs:388` 「Determination::Undetermined(why) => Err(anyhow::anyhow!(」 (P6 は backlog e8a61ec3 で解消。旧引用 `Known(None) | Undetermined(_) => Vec::new()` は撤去済み)
- `crates/condukt/src/circuit.rs:506` 「Determination::Undetermined(_) => 0,」
- `crates/playbook/src/main.rs:148` 「Determination::Undetermined(_) => return,」
- `crates/runbook/src/main.rs:122` 「Determination::Undetermined(_) => return,」
- `crates/specguard/src/scope.rs:506` 「Determination::Undetermined(_) => Vec::new(),」
- `crates/backlog/src/main.rs:2073` 「Determination::Undetermined(_) => String::new(),」
- `crates/backlog/src/main.rs:2075` 「Determination::Undetermined(_) => String::new(),」
- `crates/backlog/src/main.rs:2222` 「Determination::Undetermined(_) => None,」

<!-- /record -->
