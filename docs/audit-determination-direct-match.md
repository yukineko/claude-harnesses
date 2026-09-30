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

prod 266 件から、波括弧対応が外れた/テスト専用であることを**個別に確認して**さらに 9 件を除外した: `condukt/src/maintree.rs` の `mod tests` 内 6 件(1216,1257,1284,1298,1345,1364)、mutategate/src/lib.rs:661(mod tests 内)、`stuckguard/src/verdict_monotonicity.rs` の 2 件(main.rs:23-24 が `#[cfg(test)] mod verdict_monotonicity;` で、ファイル全体がテスト専用。『THE DEFECT』を意図的に再現する mutation 用フィクスチャ)。

**分類対象は prod の Undetermined 腕 257 件(Determination 245 / Verdict 12)**。
トリアージの『281』は test・doc を含む粗い数で、prod に絞ると 257 である(数字は継承せず測り直した)。

### 1.3 Undetermined を名指ししない直接 match

`Determination::Known(..)` だけを書き、Undetermined を wildcard / else / `matches!` に落とす形は grep `Undetermined` では拾えない。prod の `Determination::Known` pattern 行のうち、前後 25 行に Undetermined 腕が無いものを別途抽出した(§3.2)。

## 2. 集計

| 区分 | 件数 | 意味 |
|---|---:|---|
| RESTRICTIVE | 167 | block / deny / ask / error / 非0 / 明示的な unknown 表示 / 書き込み拒否 など制限側 |
| FORWARDED | 71 | Undetermined のまま上流へ返す(消費者を併記) |
| PERMISSIVE | 13 | 空・既定値・skip・allow に写す(または下流が沈黙を OK と読み得る) |
| UNCLEAR | 6 | 最終消費者まで辿れなかった |
| 合計 | 257 | |

- RESTRICTIVE のうち**有界 give-up(max_attempts 超過で Allow)**を持つのは propguard/src/gate.rs:390 と reviewgate/src/review.rs:376 の2件。どちらも stderr WARNING 付きで、初回〜max_attempts までは Block。CLAUDE.md §1 の『連続2回目の panic だけが bounded に allow』と同型の設計だが、上限値は config 依存。
- FORWARDED の最終消費者は各行に書いた。消費者側が制限側であることを確認できたものは理由欄にそう書いてある。書いていないものは『転送』までしか確認していない。

### crate 別

| crate | R | F | P | U | 計 |
|---|---:|---:|---:|---:|---:|
| autoflow | 3 | 2 | 1 | 0 | 6 |
| backlog | 10 | 4 | 3 | 0 | 17 |
| blastguard | 11 | 4 | 2 | 0 | 17 |
| budgetguard | 3 | 1 | 0 | 0 | 4 |
| condukt | 45 | 18 | 1 | 0 | 64 |
| context-governor | 0 | 0 | 0 | 1 | 1 |
| ctxrot | 2 | 0 | 0 | 5 | 7 |
| donegate | 1 | 0 | 0 | 0 | 1 |
| gauge | 4 | 0 | 0 | 0 | 4 |
| harness-status | 7 | 2 | 0 | 0 | 9 |
| mutategate | 1 | 0 | 0 | 0 | 1 |
| overwatch | 25 | 7 | 1 | 0 | 33 |
| parallelguard | 7 | 0 | 0 | 0 | 7 |
| playbook | 3 | 2 | 1 | 0 | 6 |
| propguard | 8 | 11 | 1 | 0 | 20 |
| reviewgate | 1 | 0 | 0 | 0 | 1 |
| runbook | 3 | 2 | 1 | 0 | 6 |
| schemaguard | 1 | 1 | 0 | 0 | 2 |
| session-insights | 4 | 2 | 1 | 0 | 7 |
| ship | 3 | 0 | 0 | 0 | 3 |
| specguard | 17 | 9 | 1 | 0 | 27 |
| stuckguard | 1 | 4 | 0 | 0 | 5 |
| taskprog | 4 | 1 | 0 | 0 | 5 |
| tdd | 3 | 1 | 0 | 0 | 4 |

## 3. 修正候補(PERMISSIVE / UNCLEAR)

### 3.1 PERMISSIVE(13 件)

重い順。★は下流が沈黙/既定値を OK と読み得る、または判定系の近くにあるもの。

| # | file:line | 理由と消費者 |
|---:|---|---|
| 1 | crates/blastguard/src/reversible.rs:286 | git status 判定不能 → GitState::Undetermined を作るが、decide_recovery の Repo 腕が `let _ = git;` で**読まずに捨て**(reversible.rs:174,188)RecoverableFromGit を返す。消費: detect.rs:1600 が ctx.confined_root が Some なら無人実行を許可。2026-09-18 の operator ruling(doc に明記)。消去は callsite(decide_recovery)側 |
| 2 | crates/blastguard/src/reversible.rs:288 | git status 判定不能 → GitState::Undetermined を作るが、decide_recovery の Repo 腕が `let _ = git;` で**読まずに捨て**(reversible.rs:174,188)RecoverableFromGit を返す。消費: detect.rs:1600 が ctx.confined_root が Some なら無人実行を許可。2026-09-18 の operator ruling(doc に明記)。消去は callsite(decide_recovery)側 |
| 3 | crates/autoflow/src/lock.rs:58 | find_backlog_binary 判定不能 → `return true`(driver active = 'stand down')。main.rs:170 が `return` し autoflow の Stop block 自体を行わない = この Stop は素通り。doc は restrictive と自称するが、Stop ゲートから見れば skip |
| 4 | crates/propguard/src/derive.rs:185 | criteria_file 読めず → stderr 警告のうえ **inline done_criteria があればそれで続行**(derive.rs:195-199)。ゲートは走るが、意図した基準(file)と異なる基準で評価される(既定値への fallback)。inline も空なら Undetermined を転送(205) |
| 5 | crates/playbook/src/main.rs:148 | UserPromptSubmit hook: 読めない store を『空と同じ』= 注入なしで**無言 return**(コメントが 'treated the same as an empty one' と自認)。stderr 出力なし。消費者は model のコンテキスト(注入ノートの欠落を知り得ない) |
| 6 | crates/runbook/src/main.rs:122 | playbook と同型: 読めない store を空と同じ扱いで無言 return(runbook 注入が黙って欠落) |
| 7 | crates/overwatch/src/store.rs:376 | read_jsonl_best_effort: `Known(None) \| Undetermined(_) => Vec::new()`。doc が自認する『判定不能 → 空』(store.rs:366-372)。pub wrapper 10 本(read_events/rollbacks/review_findings/bridged_*/dispositions/runtime_conflicts/review_findings_all/merge_conflicts/merge_conflict_resolutions)が消費。production の消費者を追跡した結果(specguard main.rs:2104/2169, store.rs:1602-1603 compact, 1844-1845, 1899, condukt worktree.rs:1172)はいずれも『空 = 保守側』に倒れる形だったが、wrapper は pub のまま残る(latent) |
| 8 | crates/backlog/src/main.rs:1506 | git config 読み出しの stdout 判定不能 → `String::new()`。消費: github::is_github_remote('')=false → decide_issue_create が 'remote is not github.com; left task as local-only' と**観測していない事実**を理由に DegradedLocalOnly |
| 9 | crates/backlog/src/main.rs:1508 | git の spawn/timeout 判定不能 → `String::new()`。上に同じ(remote が無いと誤記される)。呼び出し: main.rs:783(add), 1195(sync), 1584(mirror_close) |
| 10 | crates/backlog/src/main.rs:1643 | gh の spawn/timeout 判定不能 → None。消費側は None を 'gh CLI not found; left task as local-only' (github.rs:104) と記録。timeout を不在と取り違える(mirror_close は 'issue left OPEN' と警告するので黙殺ではない) |
| 11 | crates/condukt/src/circuit.rs:506 | journal の `CircuitRecord::idle_secs: i64` に判定不能を **0** で記録(コメントが 'KNOWN RESIDUAL' と自認)。stdout JSON は null だが永続 journal は 0。消費: load_circuit_records はテストからのみ(production 消費者は grep で未発見)。idle 軸 off 時は 0 が測定値と区別できない |
| 12 | crates/session-insights/src/main.rs:203 | subagent 読み不能 → stderr のみで under-count の turns を record に書き続ける(main.rs:187-188 → record::write_from_session)。ノート本文の turns 数値に不完全の注記は無い(cost 側は inline 開示=record.rs:117)。消費者は Obsidian ノート。low |
| 13 | crates/specguard/src/scope.rs:506 | relevant_file_map: code index 判定不能 → `Vec::new()`(base set のみ)。doc が『additive & advisory, 下流が短い map を clean と読まない』と主張。consumer を追跡: specguard/src/main.rs:582,674 の prompt 描画(auditor への読み順ヒント)のみで verdict 計算は見つからず。無音。low |

#### 逐語引用と消費者の追跡

**P1 crates/blastguard/src/reversible.rs:286,288 — 消去は callsite ではなく `decide_recovery`(:174)にある**

```rust
// reversible.rs:284-289  (probe)
Determination::Known(out) => match out.stdout_allowing(&[0]) {
    Determination::Known(stdout) => decide_git_state(true, true, &stdout),
    Determination::Undetermined(_) => decide_git_state(true, false, ""),   // -> GitState::Undetermined
},
Determination::Undetermined(_) => decide_git_state(false, false, ""),     // -> GitState::Undetermined

// reversible.rs:174,188  (decide_recovery)
let _ = git;
...
RepoProbe::Repo => Recovery::RecoverableFromGit,
```

`git status` が答えなかった場合でも、`RepoProbe::Repo`(work tree 内)と判定済みなら `RecoverableFromGit` になる。消費者 detect.rs:1600 は `RecoverableFromGit => ctx.confined_root("redirect", ..).is_some()` なら無人実行(プロンプト省略)を許す。doc(:135-160)に operator ruling 2026-09-18 として『`Repo` 行は git 状態を見ない』『この保護を手放す』と明記されている = **意図的な緩和**。ただし『git が答えなかった』ことまで捨てているかは ruling 文言(『gitで復元できるもの』)の射程外にも読めるため、人間判断に返す候補。

**P2 crates/autoflow/src/lock.rs:58**

```rust
Determination::Undetermined(_) => return true,   // backlog_driver_active: 'stand down'
```

消費者 autoflow/src/main.rs:170: `if lock::backlog_driver_active(&cwd) { return; }` — `stop_run` の先頭で抜け、以降の block 判定(record-requested / condukt 状態 / backlog 残)を**全て行わない** = この Stop は素通り。doc(lock.rs:35-50)は『stand down が制限側』と書くが、それは『二重駆動の回避』という意味での制限であり、Stop ゲートとしては allow。§1 の『docstring が実挙動と違う安全な話を書く』に該当する可能性があり、記述か挙動のどちらかを直す候補。

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

criteria_file が読めないとき、inline `done_criteria` が非空ならそれで `Known` を返す。ゲート自体は走る(allow ではない)が、意図した基準とは別の基準で評価される。通知は stderr のみ。inline も空なら Undetermined を転送し gate.rs:232 が `criteria-unreadable` で Block する。

**P4/P5 playbook/src/main.rs:148, runbook/src/main.rs:122**

```rust
// Fail-soft: an unreadable store is treated the same as an empty one —
// this hook must never fail a turn, so `Undetermined` just means "no
// notes to inject this time," not an error surfaced to the user.
let notes = match store.load_visible(&root) {
    harness_core::verdict::Determination::Known(n) => n,
    harness_core::verdict::Determination::Undetermined(_) => return,
};
```

UserPromptSubmit 注入 hook。読めない store を『空と同じ』と自認し、**stderr すら出さず**無言で return。消費者は model のコンテキストで、注入されるはずだったノート/runbook の欠落を知り得ない。同じ crate の CLI 側(main.rs:215,242,152,181,337,320)は `unknown — store could not be read` と明示して exit 1 しているので、hook 側だけが沈黙。コメントの『must never fail a turn』は CLAUDE.md §1 が撤去した正当化と同型。ただし hook は verdict を返さないので、少なくとも stderr での unknown 表明が最低線。

**P6 crates/overwatch/src/store.rs:376 — `read_jsonl_best_effort`**

```rust
Determination::Known(None) | Determination::Undetermined(_) => Vec::new(),
```

doc(:366-372)が『the very collapse `scan_jsonl` exists to avoid』『nothing that makes a DECISION should call one』と自認する意図的な空集合化。pub wrapper 10 本が消費する。production 消費者を全て追跡した:

| wrapper | production 消費者 | 空になったときの向き |
|---|---|---|
| `read_review_findings` | specguard/src/main.rs:2104,2169 | 既存 id 集合が空 → finding を**再 append**(重複行は可視。コメントで意図的) = 保守側 |
| `read_review_findings` / `read_bridged_findings` / `read_dispositions` | overwatch/src/store.rs:1602-1603(compaction の resolved 集合) | resolved が小さくなる → 多くが open に残る = 保守側 |
| `read_merge_conflicts` | store.rs:1844(open_merge_conflicts)→ `clear_runtime_overlap_holds`(:1368)、condukt/src/worktree.rs:1172(resolve_merge) | entry 不明 → hold を解除しない / 'no merge-conflict entry' エラー = 保守側 |
| `read_merge_conflict_resolutions` | store.rs:1845,1899 | resolution 不明 → 全 entry が open = 保守側(doc に明記) |
| `read_events` / `read_rollbacks` / `read_runtime_conflicts` / `read_review_findings_all` / `read_bridged_entries` | production 消費者は grep で**未発見**(テストのみ) | — |

現状の消費者は全て保守側に倒れるので、現時点の実害は確認できなかった。ただし wrapper は pub のままで、新しい呼び出しが判定に使えば fail-open になる(latent)。『保守側に倒れる』の根拠は消費者 5 系統のコード読みであり、テストで固定されているわけではない点に注意。

**P7/P8 crates/backlog/src/main.rs:1506,1508,1643 — 『remote が無い』『gh が無い』と観測していない事実を記録**

```rust
Determination::Undetermined(_) => String::new(),   // git_remote_origin_url (1506, 1508)
Determination::Undetermined(_) => None,            // gh_probe (1643)
```

消費: `github::decide_issue_create('')` は `is_github_remote('')==false` で `'remote is not github.com; left task as local-only'`(github.rs:96)、`gh_probe` の None は `'gh CLI not found; left task as local-only'`(github.rs:104)と**理由を断定**する。git の timeout / 非0終了も gh の timeout も同じ文言になる。`sync`/`mirror_close` は失敗を警告・非0で出すので黙殺ではないが、理由は観測に反して断定されている。`add` は local-only に degrade するだけ。

**P9 crates/condukt/src/circuit.rs:506**

```rust
let journaled_idle_secs = match &idle {
    Determination::Known(secs) => *secs,
    Determination::Undetermined(_) => 0,
};
```

コメント(:488-503)が『KNOWN RESIDUAL』と自認。stdout JSON は `null` だが、永続 journal(`CircuitRecord::idle_secs: i64`)には 0 が入る。`idle_ttl_secs==0`(idle 軸 off)のときは 0 が測定値と区別できない。`load_circuit_records` の消費者はテストのみで production 消費者は見つからなかった。修正は `Option<i64>` 化(コメントが follow-up と記載)。

**P10 crates/session-insights/src/main.rs:203** — sub-agent の turns を読めず under-count になっても、記録は書き続け、通知は stderr のみ。ノート本文の turns 数値に不完全の注記は無い(cost 側は record.rs:117 で inline 開示)。消費者は Obsidian ノート。low。

**P11 crates/specguard/src/scope.rs:506** — code index が答えなければ `Vec::new()`(base set のみ)。doc は『additive and advisory、verdict を計算しない』と主張。`relevant_file_map` の呼び出しは specguard/src/main.rs:582,674(prompt 描画)のみと確認。無音。low。doc 自身が『下流が内容から結論を出し始めたら Determination を返せ』と条件を書いているので、その条件のガードが無い点だけが残る。

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

prod の `Determination::Known` pattern 行で、前後 25 行に Undetermined 腕が無いもの 7 件を抽出し、全て読んだ。

| file:line | 形 | 分類 | 理由 |
|---|---|---|---|
| blastguard/src/retro.rs:323 | `let Determination::Known(paths) = .. else { .. }` | R | else で `dir_unreadable: true` を立てる(:324-326) |
| condukt/src/claim.rs:700 | `!matches!(progress(c), Known(Stalled))` | R | Stalled 以外(Undetermined 含む)は claim を**保持**(reap しない)。doc :688-690 |
| condukt/src/lock.rs:416 | `matches!(pid_alive(..), Known(false))` | R | owner 死亡が**積極的に観測**された時だけ reap。判定不能は待機側(:406-415) |
| hypothesis/src/lock.rs:251 | 同上 | R | 同上 |
| overwatch/src/lock.rs:290 | 同上 | R | 同上 |
| condukt/src/maintree.rs:814 | `excluded = matches!(tree_role, Known(Linked))` | R | Undetermined は excluded にならず、残りの観測(integration / staged / peers)を**実行する** = より多く検査する側 |
| specguard/src/main.rs:1365 | `Known(Some(body)) => ..` | (n/a) | 同関数 :1401 に Undetermined 腕あり(§4 に計上済み)。抽出の偽陽性 |

これらは wildcard が制限側に倒れるよう**意図して書かれている**ことをコメントとコードで確認した。ただし『将来 variant が増えても同じ』という保証は型では無い(`Determination` は 2 variant で `#[non_exhaustive]` でもなく exhaustive match になるので、新 variant は pattern 側でもコンパイルエラーになる)。

## 4. 全件表(crate 別)

凡例: R=RESTRICTIVE / F=FORWARDED / P=PERMISSIVE / U=UNCLEAR。kind は `Determination` または `Verdict`(`Verdict::Undetermined`)。
`Required::Blocked(Verdict::Undetermined(..))` の destructure(condukt/src/maintree.rs:600,614,782)は require() 経路なので既監査領域だが、Verdict 腕として数えたうえで F にしてある。

### autoflow  (R 3 / F 2 / P 1 / U 0)

| file:line | kind | 分類 | 理由 |
|---|---|:-:|---|
| src/condukt.rs:52 | D | F | load_latest の Undetermined をそのまま返す。消費: autoflow/src/main.rs:229 が block_undetermined で block |
| src/condukt.rs:133 | D | R | running マーク失敗を stderr に明示。判定ではなく簿記で、次の Stop が再観測する |
| src/condukt.rs:173 | D | F | latest_run_file の Undetermined を上流へ転送 |
| src/lock.rs:58 | D | P | find_backlog_binary 判定不能 → `return true`(driver active = 'stand down')。main.rs:170 が `return` し autoflow の Stop block 自体を行わない = この Stop は素通り。doc は restrictive と自称するが、Stop ゲートから見れば skip |
| src/lock.rs:125 | D | R | 判定不能 → false(再開マーカーを書かない)。消費: pre_compact_run が return するだけ。検証ではなく便宜機能 |
| src/main.rs:229 | D | R | condukt run-state 判定不能 → block_undetermined(Phase を据え置き) |

### backlog  (R 10 / F 4 / P 3 / U 0)

| file:line | kind | 分類 | 理由 |
|---|---|:-:|---|
| src/claim_ledger.rs:392 | D | F | ledger lock / read の Undetermined を Ok(Undetermined) で転送。消費: backlog/src/main.rs:1056 が REFUSED エラー |
| src/claim_ledger.rs:398 | D | F | ledger lock / read の Undetermined を Ok(Undetermined) で転送。消費: backlog/src/main.rs:1056 が REFUSED エラー |
| src/driver.rs:206 | D | F | driver 登録 dir の列挙失敗を転送。消費: liveness.rs status_value が undetermined を描画 |
| src/driver.rs:215 | D | F | driver 登録 dir の列挙失敗を転送。消費: liveness.rs status_value が undetermined を描画 |
| src/liveness.rs:64 | D | R | progress を 'undetermined' と文字列化 |
| src/liveness.rs:110 | D | R | presence を None にするが、直後 (liveness.rs:154-165) で `kind: undetermined, undetermined: true` を `stale` 無しで返し、既存パーサは active と読む |
| src/lock.rs:423 | D | R | 他者 lock の診断メッセージに 'undetermined: why' を出す。lock は取得しない(contended) |
| src/lock.rs:612 | D | R | signal を readable:false + 理由でレポート |
| src/lock.rs:677 | D | R | probe 結果 verdict='undetermined', reap_eligible=false |
| src/main.rs:652 | D | R | Err(REFUSED: claim ledger 不明) で終了 |
| src/main.rs:1056 | D | R | `backlog next --claim REFUSED` で Err。'no pending tasks' と区別 |
| src/main.rs:1454 | D | R | JSON に `undetermined: true` + reason を出力 |
| src/main.rs:1506 | D | P | git config 読み出しの stdout 判定不能 → `String::new()`。消費: github::is_github_remote('')=false → decide_issue_create が 'remote is not github.com; left task as local-only' と**観測していない事実**を理由に DegradedLocalOnly |
| src/main.rs:1508 | D | P | git の spawn/timeout 判定不能 → `String::new()`。上に同じ(remote が無いと誤記される)。呼び出し: main.rs:783(add), 1195(sync), 1584(mirror_close) |
| src/main.rs:1637 | D | R | `unreachable!`。stdout_allowing(&[code]) は定義上 Known (boundary.rs:205)。万一到達すれば panic = 大きく落ちる |
| src/main.rs:1643 | D | P | gh の spawn/timeout 判定不能 → None。消費側は None を 'gh CLI not found; left task as local-only' (github.rs:104) と記録。timeout を不在と取り違える(mirror_close は 'issue left OPEN' と警告するので黙殺ではない) |
| src/store.rs:2070 | D | R | eprintln 警告 + `unresolved: true` のラベルで返す(検索で見えにくくなる旨を明示) |

### blastguard  (R 11 / F 4 / P 2 / U 0)

| file:line | kind | 分類 | 理由 |
|---|---|:-:|---|
| src/approve.rs:215 | D | F | operand 位置/probe/pending の判定不能を Determination::undetermined で再送。消費: main.rs:665 が Decision::Ask |
| src/approve.rs:233 | D | F | operand 位置/probe/pending の判定不能を Determination::undetermined で再送。消費: main.rs:665 が Decision::Ask |
| src/approve.rs:304 | D | R | Lookup::Undetermined を返す(is_approved は true にならない) |
| src/approve.rs:357 | D | F | operand 位置/probe/pending の判定不能を Determination::undetermined で再送。消費: main.rs:665 が Decision::Ask |
| src/detect.rs:263 | D | R | classify 判定不能 → None(緩和なし = 元の Deny/Ask を維持。緩和専用の関数) |
| src/detect.rs:1258 | V | R | 最初の Undetermined を rest に積み、最終的に worst_of → Ask |
| src/detect.rs:1267 | V | R | `matches!(r, Verdict::Undetermined(_))` は重複排除のみ |
| src/detect.rs:1301 | V | R | Verdict::Undetermined → Decision::ask |
| src/detect.rs:7557 | D | R | worktree_confined: Undetermined → false(緩和しない) |
| src/exclude.rs:572 | D | R | allowlist コンパイル不能 → false(exempt を拒否 = ルールに回す) |
| src/exclude.rs:601 | D | R | 保護パス集合がコンパイル不能 → true(全 operand を保護対象と扱い over-deny) |
| src/exclude.rs:659 | D | R | 保護パス集合がコンパイル不能 → true(全 operand を保護対象と扱い over-deny) |
| src/main.rs:665 | D | R | fingerprint 不能 → Decision::Ask のまま(承認は適用されない) |
| src/main.rs:841 | D | F | ディレクトリ走査不完全を Determination::undetermined で再送 |
| src/retro.rs:355 | D | R | transcript 読めず → failed_file()(レビューが見なかった旨を計上) |
| src/reversible.rs:286 | D | P | git status 判定不能 → GitState::Undetermined を作るが、decide_recovery の Repo 腕が `let _ = git;` で**読まずに捨て**(reversible.rs:174,188)RecoverableFromGit を返す。消費: detect.rs:1600 が ctx.confined_root が Some なら無人実行を許可。2026-09-18 の operator ruling(doc に明記)。消去は callsite(decide_recovery)側 |
| src/reversible.rs:288 | D | P | git status 判定不能 → GitState::Undetermined を作るが、decide_recovery の Repo 腕が `let _ = git;` で**読まずに捨て**(reversible.rs:174,188)RecoverableFromGit を返す。消費: detect.rs:1600 が ctx.confined_root が Some なら無人実行を許可。2026-09-18 の operator ruling(doc に明記)。消去は callsite(decide_recovery)側 |

### budgetguard  (R 3 / F 1 / P 0 / U 0)

| file:line | kind | 分類 | 理由 |
|---|---|:-:|---|
| src/config.rs:124 | D | F | config 位置の Undetermined を転送。消費: main.rs:158 が config_undetermined_result で block |
| src/gate.rs:106 | D | R | コスト不明 → undetermined_verdict(GateResult)。日次 ledger に書かない |
| src/main.rs:158 | D | R | config 判定不能 → block(Stop hook 内、stop_hook_active で有界) |
| src/main.rs:231 | D | R | CLI 表示で threshold/pressure を出さず unknown と明示 |

### condukt  (R 45 / F 18 / P 1 / U 0)

| file:line | kind | 分類 | 理由 |
|---|---|:-:|---|
| src/circuit.rs:296 | D | F | transcript mtime 不能を理由付きで再送。消費: circuit verdict が idle 不明時 trip(idle_unmeasured) |
| src/circuit.rs:340 | D | R | JSON: idle_secs=null + idle_unknown_reason |
| src/circuit.rs:506 | D | P | journal の `CircuitRecord::idle_secs: i64` に判定不能を **0** で記録(コメントが 'KNOWN RESIDUAL' と自認)。stdout JSON は null だが永続 journal は 0。消費: load_circuit_records はテストからのみ(production 消費者は grep で未発見)。idle 軸 off 時は 0 が測定値と区別できない |
| src/claim.rs:561 | D | F | progress_store_dir 不能 → Undetermined 転送。retain_claim は Stalled 以外(= Undetermined 含む)を保持(claim.rs:700) |
| src/claim.rs:655 | D | F | worktree HEAD 1 つでも読めなければ run-scoped signal 全体を Undetermined に |
| src/claim.rs:786 | D | R | project root 不能 → 全 file/hashkey を skipped(呼び出し側が task を hard-skip) |
| src/claim.rs:880 | D | R | project root 不能 → 全 file/hashkey を skipped(呼び出し側が task を hard-skip) |
| src/claim.rs:946 | D | R | project root 不能 → project_undetermined_err で Err(空 Registry を返さない) |
| src/claim.rs:978 | D | R | project root 不能 → project_undetermined_err で Err(空 Registry を返さない) |
| src/claim.rs:1011 | D | R | project root 不能 → project_undetermined_err で Err(空 Registry を返さない) |
| src/claim.rs:1041 | D | R | project root 不能 → project_undetermined_err で Err(空 Registry を返さない) |
| src/claim.rs:1081 | D | R | project root 不能 → project_undetermined_err で Err(空 Registry を返さない) |
| src/claim.rs:1174 | D | R | RunLiveness::Undetermined。消費: worktree.rs:865 が Live と同じく true(merge hold 維持) |
| src/claim.rs:1235 | D | F | transcript 位置不能を理由付きで再送 |
| src/claim.rs:1285 | D | R | StatelessIdle::RegistryUnreadable。消費: circuit.rs:223 が Determination::undetermined に写す |
| src/claim.rs:1342 | D | F | session mtime 不能を Measured(Undetermined) で返す |
| src/claim.rs:1528 | D | R | project root 不能 → project_undetermined_err で Err(空 Registry を返さない) |
| src/gate_exec.rs:120 | D | R | GatherOutcome::Undetermined。消費: decide_from_outcome が GateExec::Escalate + 理由(gate_exec.rs:210-224) |
| src/main.rs:5133 | D | R | bail! で失敗(推測したパスを出さない) |
| src/main.rs:5151 | D | R | bail! で失敗(推測したパスを出さない) |
| src/main.rs:5293 | D | R | エラー文に 'could not be read' と明示 |
| src/main.rs:5467 | D | F | load_parsed_decomposition の Undetermined を転送(再 mint しない) |
| src/maintree.rs:263 | V | R | `_ => 1` の wildcard だが Undetermined は exit 2。全 verdict が Clean 以外 block |
| src/maintree.rs:600 | V | F | `Required::Blocked(Verdict::Undetermined(r))` = require() の destructure。Determination::Undetermined に写して転送(require() 監査済み領域の再掲) |
| src/maintree.rs:607 | D | F | parse 結果の Undetermined を転送 |
| src/maintree.rs:614 | V | F | `Required::Blocked(Verdict::Undetermined(r))` = require() の destructure。Determination::Undetermined に写して転送(require() 監査済み領域の再掲) |
| src/maintree.rs:622 | D | F | parse 結果の Undetermined を転送 |
| src/maintree.rs:782 | V | F | `Required::Blocked(Verdict::Undetermined(r))` = require() の destructure。Determination::Undetermined に写して転送(require() 監査済み領域の再掲) |
| src/maintree.rs:876 | V | R | JSON verdict='undetermined' |
| src/maintree.rs:907 | V | R | 'UNDETERMINED ... so this blocks.' を表示(block) |
| src/shadow_run.rs:90 | D | R | bail!: branch が判定不能なら shadow worktree を破棄しない |
| src/state.rs:1383 | D | R | scan.undetermined に積む(stuck 判定に使わない) |
| src/state.rs:1448 | D | R | Some(why) = keep reason(判定不能なら abandon しない) |
| src/state.rs:1504 | D | R | 'undetermined' / readable:false と表示 |
| src/state.rs:1515 | D | R | 'undetermined' / readable:false と表示 |
| src/state.rs:1595 | D | F | progress 判定不能を転送 |
| src/verify.rs:1680 | V | R | Verdict::Undetermined → ChecksVerdict::NoChecksDeclared(Passed でない。verify.rs:1841 `all_passed: verdict == Passed` と :4051 のテストで false を確認) |
| src/worktree.rs:895 | D | R | bail!(merge / remove を拒否) |
| src/worktree.rs:2515 | D | R | bail!(merge / remove を拒否) |
| src/wt_reap.rs:131 | D | R | Reap::Undetermined。消費: wt_reap.rs:305 が KeptUndetermined(削除しない) |
| src/wt_reap.rs:136 | D | R | Reap::Undetermined。消費: wt_reap.rs:305 が KeptUndetermined(削除しない) |
| src/wt_reap.rs:143 | D | R | Reap::Undetermined。消費: wt_reap.rs:305 が KeptUndetermined(削除しない) |
| src/wt_reap.rs:153 | D | R | Reap::Undetermined。消費: wt_reap.rs:305 が KeptUndetermined(削除しない) |
| src/wt_reconcile.rs:291 | D | R | 削除候補判定: Undetermined → false(削除しない) |
| src/wt_reconcile.rs:294 | D | R | 削除候補判定: Undetermined → false(削除しない) |
| src/wt_reconcile.rs:297 | D | R | 削除候補判定: Undetermined → false(削除しない) |
| src/wt_reconcile.rs:355 | D | R | ResumeVerdict::Undetermined('undetermined is not an offer') |
| src/wt_reconcile.rs:362 | D | R | ResumeVerdict::Undetermined('undetermined is not an offer') |
| src/wt_reconcile.rs:365 | D | R | ResumeVerdict::Undetermined('undetermined is not an offer') |
| src/wt_reconcile.rs:429 | D | R | Judged::undetermined / 'undetermined' として報告に残す |
| src/wt_reconcile.rs:435 | D | R | Judged::undetermined / 'undetermined' として報告に残す |
| src/wt_reconcile.rs:441 | D | R | Judged::undetermined / 'undetermined' として報告に残す |
| src/wt_reconcile.rs:447 | D | R | Judged::undetermined / 'undetermined' として報告に残す |
| src/wt_reconcile.rs:1109 | D | F | home / registry 位置の Undetermined を転送 |
| src/wt_reconcile.rs:1302 | D | F | home / registry 位置の Undetermined を転送 |
| src/wt_reconcile.rs:1404 | D | F | home / registry 位置の Undetermined を転送 |
| src/wt_reconcile.rs:1443 | D | F | occupancy 判定の入力が不明 → Determination::undetermined を再送(死亡と断定しない) |
| src/wt_reconcile.rs:1463 | D | F | occupancy 判定の入力が不明 → Determination::undetermined を再送(死亡と断定しない) |
| src/wt_reconcile.rs:1470 | D | F | occupancy 判定の入力が不明 → Determination::undetermined を再送(死亡と断定しない) |
| src/wt_reconcile.rs:1483 | D | R | Judged::undetermined / 'undetermined' として報告に残す |
| src/wt_reconcile.rs:1820 | D | R | 判定不能の理由を説明文に出す |
| src/wt_reconcile.rs:1829 | D | R | 判定不能の理由を説明文に出す |
| src/wt_reconcile.rs:1837 | D | R | 判定不能の理由を説明文に出す |
| src/wt_reconcile.rs:1916 | D | R | bail!(progress store 不明なら liveness を判定しない) |

### context-governor  (R 0 / F 0 / P 0 / U 1)

| file:line | kind | 分類 | 理由 |
|---|---|:-:|---|
| src/backing.rs:93 | D | U | snapshot を保存せず stderr に 'なかった のではない' と明記してスキップ。ただし hook の stderr が人間/消費者に届くかは未検証、判定を消費する下流も見つからず(observability) |

### ctxrot  (R 2 / F 0 / P 0 / U 5)

| file:line | kind | 分類 | 理由 |
|---|---|:-:|---|
| src/hooks/distill.rs:133 | D | U | 蒸留をスキップ。stderr に 'なかった のではない' と明記してスキップ。ただし hook の stderr が人間/消費者に届くかは未検証、判定を消費する下流も見つからず(observability) |
| src/hooks/guard.rs:547 | D | U | 再浮上なし(return None)。stderr に 'なかった のではない' と明記してスキップ。ただし hook の stderr が人間/消費者に届くかは未検証、判定を消費する下流も見つからず(observability) |
| src/hooks/rescue.rs:61 | D | R | 既存ノート確認不能 → 重複を許して**書く**(保存側に倒す) |
| src/hooks/rescue.rs:78 | D | U | 退避ノートを作らない。stderr に 'なかった のではない' と明記してスキップ。ただし hook の stderr が人間/消費者に届くかは未検証、判定を消費する下流も見つからず(observability) |
| src/hooks/restore.rs:69 | D | U | 引き継ぎを注入しない(None)。stderr に 'なかった のではない' と明記してスキップ。ただし hook の stderr が人間/消費者に届くかは未検証、判定を消費する下流も見つからず(observability) |
| src/hooks/restore.rs:82 | D | U | 引き継ぎを注入しない(None)。stderr に 'なかった のではない' と明記してスキップ。ただし hook の stderr が人間/消費者に届くかは未検証、判定を消費する下流も見つからず(observability) |
| src/main.rs:309 | D | R | known_or_exit: stderr 'ノート無しではありません' + exit(1) |

### donegate  (R 1 / F 0 / P 0 / U 0)

| file:line | kind | 分類 | 理由 |
|---|---|:-:|---|
| src/gate.rs:245 | D | R | 'git state undetermined ... all checks ran unscoped, fail-closed'(スコープを広げて検査) |

### gauge  (R 4 / F 0 / P 0 / U 0)

| file:line | kind | 分類 | 理由 |
|---|---|:-:|---|
| src/main.rs:214 | D | R | under-count の記録を書かない。消費: budgetguard session_cost は record 不在なら transcript 見積へ fallback(gate.rs:326-339) |
| src/main.rs:506 | D | R | stderr + exit(1) / status:unknown |
| src/main.rs:576 | D | R | stderr + exit(1) / status:unknown |
| src/main.rs:620 | D | R | 'unknown (...)' と表示 |

### harness-status  (R 7 / F 2 / P 0 / U 0)

| file:line | kind | 分類 | 理由 |
|---|---|:-:|---|
| src/display.rs:63 | D | R | 'unknown — ...' / status:unknown を明示 |
| src/display.rs:177 | D | R | 'unknown — ...' / status:unknown を明示 |
| src/display.rs:211 | D | R | 'unknown — ...' / status:unknown を明示 |
| src/display.rs:222 | D | R | 'unknown — ...' / status:unknown を明示 |
| src/main.rs:136 | D | R | unknown を明示、非0終了 or 判定不能セクション |
| src/main.rs:274 | D | R | unknown を明示、非0終了 or 判定不能セクション |
| src/main.rs:319 | D | R | unknown を明示、非0終了 or 判定不能セクション |
| src/path_shadow.rs:155 | D | F | scan 判定不能を転送 |
| src/sessions.rs:45 | D | F | session store 不能を転送 |

### mutategate  (R 1 / F 0 / P 0 / U 0)

| file:line | kind | 分類 | 理由 |
|---|---|:-:|---|
| src/main.rs:77 | D | R | outcomes 読めず → exit 2 |

### overwatch  (R 25 / F 7 / P 1 / U 0)

| file:line | kind | 分類 | 理由 |
|---|---|:-:|---|
| src/aggregate.rs:363 | D | R | SourceOutput::Undetermined('NOT a report that {cmd} is empty') → view.mark_undetermined (aggregate.rs:459-489) |
| src/aggregate.rs:562 | D | R | ledger mtime 不能 → fresh=false(再構築する) |
| src/bridge.rs:272 | D | R | 空 Vec を返すが WARNING を出し `undetermined` 配列に ledger 名を積む(Vec::new() の裏に記録あり) |
| src/bridge.rs:297 | D | R | idempotency ledger 不明 → その stream を**スキップ**(None) |
| src/disposition_cli.rs:116 | D | R | rate を計算せず WARNING + undetermined に積む |
| src/disposition_cli.rs:131 | D | R | rate を計算せず WARNING + undetermined に積む |
| src/disposition_cli.rs:224 | D | R | None + undetermined.push('stale-undisposed join') |
| src/gate_outcomes.rs:244 | D | F | read 不能を転送(再 mint しない) |
| src/gate_outcomes.rs:276 | D | R | any_undetermined=true |
| src/gate_outcomes.rs:285 | D | R | JSON status='undetermined' / 'UNDETERMINED: log could not be read' |
| src/gate_outcomes.rs:364 | D | R | JSON status='undetermined' / 'UNDETERMINED: log could not be read' |
| src/reconcile.rs:208 | D | R | WARNING + undetermined に積み、何も reconcile しない(NO finding was reconciled) |
| src/reconcile.rs:233 | D | R | WARNING + undetermined に積み、何も reconcile しない(NO finding was reconciled) |
| src/review_escalation.rs:178 | D | F | read 不能を転送 |
| src/review_queue.rs:705 | D | R | 空 Vec だが UndeterminedSource(effect=Omitted)を sink に積み、marker row として描画 |
| src/review_queue.rs:852 | D | R | UndeterminedSource(effect=ShownUnfiltered)を積む(フィルタ無しで全件表示 = 隠さない側) |
| src/store.rs:83 | D | R | bail!(storage root 不明) |
| src/store.rs:204 | D | R | bail!(leases.json 読めず) |
| src/store.rs:362 | D | F | scan_jsonl の Undetermined を転送(再 mint しない) |
| src/store.rs:376 | D | P | read_jsonl_best_effort: `Known(None) \| Undetermined(_) => Vec::new()`。doc が自認する『判定不能 → 空』(store.rs:366-372)。pub wrapper 10 本(read_events/rollbacks/review_findings/bridged_*/dispositions/runtime_conflicts/review_findings_all/merge_conflicts/merge_conflict_resolutions)が消費。production の消費者を追跡した結果(specguard main.rs:2104/2169, store.rs:1602-1603 compact, 1844-1845, 1899, condukt worktree.rs:1172)はいずれも『空 = 保守側』に倒れる形だったが、wrapper は pub のまま残る(latent) |
| src/store.rs:708 | D | F | ReviewFindingScan::Undetermined へ写して返す |
| src/store.rs:765 | D | R | AppendOutcome::SkippedUndetermined(書かない。'NOT appended' を明示) |
| src/store.rs:878 | D | R | AppendOutcome::SkippedUndetermined(書かない。'NOT appended' を明示) |
| src/store.rs:1119 | D | R | AppendOutcome::SkippedUndetermined(書かない。'NOT appended' を明示) |
| src/store.rs:1203 | D | R | bail!('in-flight set is unknown, not empty') |
| src/store.rs:1471 | D | F | hot/archive どちらかが Undetermined なら全体を Undetermined で転送 |
| src/store.rs:1472 | D | F | hot/archive どちらかが Undetermined なら全体を Undetermined で転送 |
| src/store.rs:1594 | D | R | bail!(compaction を書き込み前に中止) |
| src/store.rs:1621 | D | R | bail!(compaction を書き込み前に中止) |
| src/store.rs:1707 | D | R | bail!(merge conflict 記録せず) |
| src/store.rs:1783 | D | R | AppendOutcome::SkippedUndetermined(書かない。'NOT appended' を明示) |
| src/store.rs:1883 | D | R | resolution 不明 → 空 resolutions と共に `Some(why)` を構造体に保持 = 全 entry を open と報告(隠さない側) |
| src/undetermined_metrics.rs:105 | D | F | 転送(再記録しない) |

### parallelguard  (R 7 / F 0 / P 0 / U 0)

| file:line | kind | 分類 | 理由 |
|---|---|:-:|---|
| src/main.rs:175 | D | R | Decision::Deny(undetermined_reason) |
| src/main.rs:183 | D | R | Decision::Deny(undetermined_reason) |
| src/main.rs:290 | D | R | release 不能 → 'slot held until reset'(枠を解放しない = 制限側) |
| src/main.rs:300 | D | R | release 不能 → 'slot held until reset'(枠を解放しない = 制限側) |
| src/main.rs:388 | D | R | 'UNKNOWN' / 'UNREADABLE' と表示 |
| src/main.rs:419 | D | R | 'UNKNOWN' / 'UNREADABLE' と表示 |
| src/main.rs:440 | D | R | 'UNKNOWN' / 'UNREADABLE' と表示 |

### playbook  (R 3 / F 2 / P 1 / U 0)

| file:line | kind | 分類 | 理由 |
|---|---|:-:|---|
| src/main.rs:148 | D | P | UserPromptSubmit hook: 読めない store を『空と同じ』= 注入なしで**無言 return**(コメントが 'treated the same as an empty one' と自認)。stderr 出力なし。消費者は model のコンテキスト(注入ノートの欠落を知り得ない) |
| src/main.rs:215 | D | R | stderr 'unknown' + exit(1) |
| src/main.rs:242 | D | R | stderr 'unknown' + exit(1) |
| src/main.rs:337 | D | R | 'unknown — store could not be read' |
| src/store.rs:116 | D | F | 転送 |
| src/store.rs:122 | D | F | 転送 |

### propguard  (R 8 / F 11 / P 1 / U 0)

| file:line | kind | 分類 | 理由 |
|---|---|:-:|---|
| src/config.rs:283 | D | R | cfg.load_error を立てる。gate::evaluate が fail closed(config.rs:11 doc / :267 コメント) |
| src/derive.rs:185 | D | P | criteria_file 読めず → stderr 警告のうえ **inline done_criteria があればそれで続行**(derive.rs:195-199)。ゲートは走るが、意図した基準(file)と異なる基準で評価される(既定値への fallback)。inline も空なら Undetermined を転送(205) |
| src/derive.rs:205 | D | F | unreadable を保持し inline も空なら転送。消費: gate.rs:232 が Block('criteria-unreadable') |
| src/gate.rs:122 | D | R | filter list コンパイル不能 → リスト全体を無視して**全変更ファイルを検査** |
| src/gate.rs:232 | D | R | Decision::Block(criteria-unreadable) |
| src/gate.rs:283 | D | R | decide_diff_failed(fail closed, 有界) |
| src/gate.rs:390 | D | R | checker 未実行 → attempts <= max_attempts の間 Block。**max_attempts 超過で Allow('checker-error-giveup') + stderr WARNING**(有界 give-up = 設計上の permissive 出口) |
| src/gate.rs:612 | D | R | fleet outage scan 不能 → outage give-up の Allow だけを Block に格上げ。他の decision は素通し(doc 明記) |
| src/gate.rs:1053 | D | F | checker 出力/起動の判定不能を Determination::undetermined で再送 |
| src/gate.rs:1059 | D | F | checker 出力/起動の判定不能を Determination::undetermined で再送 |
| src/git.rs:106 | D | F | spawn/timeout の理由を転送 |
| src/git.rs:201 | D | R | collect: false → ChangeScan::Failed(制限側。doc に明記) |
| src/git.rs:267 | D | F | diff_text: 1 つでも読めなければ diff 全体を Undetermined で転送(空 diff にしない) |
| src/git.rs:268 | D | F | diff_text: 1 つでも読めなければ diff 全体を Undetermined で転送(空 diff にしない) |
| src/git.rs:270 | D | F | diff_text: 1 つでも読めなければ diff 全体を Undetermined で転送(空 diff にしない) |
| src/git.rs:273 | D | F | diff_text: 1 つでも読めなければ diff 全体を Undetermined で転送(空 diff にしない) |
| src/git.rs:296 | D | F | diff_text: 1 つでも読めなければ diff 全体を Undetermined で転送(空 diff にしない) |
| src/git.rs:325 | D | F | diff_text: 1 つでも読めなければ diff 全体を Undetermined で転送(空 diff にしない) |
| src/git.rs:355 | D | F | diff_text: 1 つでも読めなければ diff 全体を Undetermined で転送(空 diff にしない) |
| src/main.rs:572 | D | R | 'UNREADABLE ... every stop is BLOCKED' |

### reviewgate  (R 1 / F 0 / P 0 / U 0)

| file:line | kind | 分類 | 理由 |
|---|---|:-:|---|
| src/review.rs:376 | V | R | attempts <= max_attempts の間 Block。**超過で Allow('reviewer-error-giveup') + stderr WARNING**(有界 give-up) |

### runbook  (R 3 / F 2 / P 1 / U 0)

| file:line | kind | 分類 | 理由 |
|---|---|:-:|---|
| src/main.rs:122 | D | P | playbook と同型: 読めない store を空と同じ扱いで無言 return(runbook 注入が黙って欠落) |
| src/main.rs:152 | D | R | stderr 'unknown' + exit(1) |
| src/main.rs:181 | D | R | stderr 'unknown' + exit(1) |
| src/main.rs:320 | D | R | 'unknown — store could not be read' |
| src/store.rs:86 | D | F | 転送 |
| src/store.rs:97 | D | F | 転送 |

### schemaguard  (R 1 / F 1 / P 0 / U 0)

| file:line | kind | 分類 | 理由 |
|---|---|:-:|---|
| src/main.rs:248 | V | R | exit 2 + valid:false |
| src/metrics.rs:117 | D | F | 転送(部分和を事実として出さない) |

### session-insights  (R 4 / F 2 / P 1 / U 0)

| file:line | kind | 分類 | 理由 |
|---|---|:-:|---|
| src/main.rs:203 | D | P | subagent 読み不能 → stderr のみで under-count の turns を record に書き続ける(main.rs:187-188 → record::write_from_session)。ノート本文の turns 数値に不完全の注記は無い(cost 側は inline 開示=record.rs:117)。消費者は Obsidian ノート。low |
| src/main.rs:286 | D | R | stderr 'unknown' + exit(1) |
| src/main.rs:315 | D | R | stderr 'unknown' + exit(1) |
| src/main.rs:374 | D | R | 'sessions: unknown' |
| src/metrics.rs:203 | D | F | 転送 |
| src/metrics.rs:212 | D | F | 転送 |
| src/record.rs:117 | D | R | Some(why) = cost 不完全をノートに inline 開示 |

### ship  (R 3 / F 0 / P 0 / U 0)

| file:line | kind | 分類 | 理由 |
|---|---|:-:|---|
| src/checklist.rs:54 | D | R | stale_crates 判定不能 → true(要ゲート / blocking 扱い) |
| src/checklist.rs:124 | D | R | '[?] stale-crate check could not run' / '未完了' |
| src/checklist.rs:282 | D | R | '[?] stale-crate check could not run' / '未完了' |

### specguard  (R 17 / F 9 / P 1 / U 0)

| file:line | kind | 分類 | 理由 |
|---|---|:-:|---|
| src/coverage.rs:79 | D | R | verdict_token = UNDETERMINED |
| src/coverage.rs:189 | D | R | 'これは「正典が無い」ではない' と明示 |
| src/forge/main.rs:457 | D | R | backlog に積めなかったと stderr、非0 系 |
| src/forge/queue.rs:185 | D | F | 起票前の判定不能を Determination::undetermined で再送(積まない) |
| src/forge/queue.rs:203 | D | F | 起票前の判定不能を Determination::undetermined で再送(積まない) |
| src/forge/queue.rs:212 | D | F | 起票前の判定不能を Determination::undetermined で再送(積まない) |
| src/forge/queue.rs:244 | D | F | 起票前の判定不能を Determination::undetermined で再送(積まない) |
| src/main.rs:845 | D | R | EXIT_UNRATIFIED / ack を拒否(fail closed) |
| src/main.rs:992 | D | R | baseline を据え置く(進めない) |
| src/main.rs:1098 | D | R | baseline を据え置く(進めない) |
| src/main.rs:1295 | D | R | reason を JSON へ、EXIT_BRIEF_UNDETERMINED |
| src/main.rs:1301 | D | R | reason を JSON へ、EXIT_BRIEF_UNDETERMINED |
| src/main.rs:1330 | D | R | 『確認できませんでした』を表示(沈黙しない) |
| src/main.rs:1401 | D | R | 『確認できませんでした』を表示(沈黙しない) |
| src/main.rs:1510 | D | R | EXIT_UNRATIFIED / ack を拒否(fail closed) |
| src/main.rs:1553 | D | R | --force 時のみ到達。stderr で特定不能を明示し空 covered で続行(disposition を残さない旨を別途表示) |
| src/main.rs:1698 | D | R | EXIT_INDEX_UNDETERMINED(map を保存しない) |
| src/main.rs:1705 | D | R | EXIT_INDEX_UNDETERMINED(map を保存しない) |
| src/main.rs:1818 | D | R | EXIT_INDEX_UNDETERMINED(map を保存しない) |
| src/main.rs:1840 | D | R | EXIT_INDEX_UNDETERMINED(map を保存しない) |
| src/ratify.rs:138 | D | F | lock 判定不能を再送 |
| src/ratify.rs:186 | D | R | bail!('refusing to auto-ratify') |
| src/scope.rs:434 | D | F | code index 判定不能を Determination::undetermined で再送 |
| src/scope.rs:440 | D | F | code index 判定不能を Determination::undetermined で再送 |
| src/scope.rs:506 | D | P | relevant_file_map: code index 判定不能 → `Vec::new()`(base set のみ)。doc が『additive & advisory, 下流が短い map を clean と読まない』と主張。consumer を追跡: specguard/src/main.rs:582,674 の prompt 描画(auditor への読み順ヒント)のみで verdict 計算は見つからず。無音。low |
| src/specmap.rs:295 | D | F | enrich 判定不能を再送(map を保存しない) |
| src/specmap.rs:301 | D | F | enrich 判定不能を再送(map を保存しない) |

### stuckguard  (R 1 / F 4 / P 0 / U 0)

| file:line | kind | 分類 | 理由 |
|---|---|:-:|---|
| src/anchor.rs:139 | D | F | AnchorLookup::Undetermined へ写して転送 |
| src/anchor.rs:198 | D | F | AnchorLookup::Undetermined へ写して転送 |
| src/anchor.rs:215 | D | F | AnchorLookup::Undetermined へ写して転送 |
| src/anchor.rs:243 | D | F | AnchorLookup::Undetermined へ写して転送 |
| src/main.rs:151 | D | R | 履歴不明 → nudge を出して停止(history を上書きしない) |

### taskprog  (R 4 / F 1 / P 0 / U 0)

| file:line | kind | 分類 | 理由 |
|---|---|:-:|---|
| src/config.rs:84 | D | F | progress file 位置の判定不能を再送 |
| src/main.rs:78 | D | R | 'taskprog could not locate one' を注入(空注入にしない) |
| src/main.rs:110 | D | R | stderr + exit(1) |
| src/main.rs:175 | D | R | '(undetermined: ...)' と表示 |
| src/update.rs:49 | D | R | model チャネルに『書かなかった』と明示 |

### tdd  (R 3 / F 1 / P 0 / U 0)

| file:line | kind | 分類 | 理由 |
|---|---|:-:|---|
| src/gate.rs:90 | D | F | Report.scan の Undetermined を Verdict::Undetermined へ転送(Clean にしない) |
| src/gate.rs:242 | D | R | 'git-scan-undetermined' / '🔴 couldn't determine ... Not allowing the stop blindly' / 'BLOCKS the stop' |
| src/gate.rs:254 | D | R | 'git-scan-undetermined' / '🔴 couldn't determine ... Not allowing the stop blindly' / 'BLOCKS the stop' |
| src/gate.rs:343 | D | R | 'git-scan-undetermined' / '🔴 couldn't determine ... Not allowing the stop blindly' / 'BLOCKS the stop' |

## 5. この監査が主張しないこと(限界)

- **局所 enum の `Undetermined`** は対象外。`Reap::Undetermined`・`ResumeVerdict::Undetermined`・`RunLiveness::Undetermined`・`GatherOutcome::Undetermined`・`AnchorLookup::Undetermined`・`SourceOutput::Undetermined`・`Lookup::Undetermined` 等(`^\s*Undetermined\b` で 20 定義を確認)。ただし上の表の FORWARDED/RESTRICTIVE の判断に必要な範囲では、その消費側(wt_reap.rs:305、worktree.rs:865、gate_exec.rs:210-224、circuit.rs:223)を読んだ。全局所 enum の消費側の網羅監査は別タスク。
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

## 7. 機械検証つき逐語引用

docs の check-doc-claims.py は、バッククォートで囲んだ パス:行 とその直後の引用を index と照合する。本書の他の箇所は誤検知(短縮パス・複数行指定・同一行の別引用)を避けるためバッククォートを外してあるので、主要な PERMISSIVE 箇所だけをここで機械検証可能な形で固定する。

- `crates/blastguard/src/reversible.rs:174` 「let _ = git;」
- `crates/blastguard/src/reversible.rs:188` 「RepoProbe::Repo => Recovery::RecoverableFromGit,」
- `crates/autoflow/src/lock.rs:58` 「Determination::Undetermined(_) => return true,」
- `crates/overwatch/src/store.rs:376` 「Determination::Known(None) | Determination::Undetermined(_) => Vec::new(),」
- `crates/condukt/src/circuit.rs:506` 「Determination::Undetermined(_) => 0,」
- `crates/playbook/src/main.rs:148` 「Determination::Undetermined(_) => return,」
- `crates/runbook/src/main.rs:122` 「Determination::Undetermined(_) => return,」
- `crates/specguard/src/scope.rs:506` 「Determination::Undetermined(_) => Vec::new(),」
- `crates/backlog/src/main.rs:1700` 「Determination::Undetermined(_) => String::new(),」
- `crates/backlog/src/main.rs:1702` 「Determination::Undetermined(_) => String::new(),」
- `crates/backlog/src/main.rs:1837` 「Determination::Undetermined(_) => None,」

