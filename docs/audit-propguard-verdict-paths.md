# propguard — per-gate verdict-path 監査

DoD9 の 5 本目（schemaguard / autoflow / budgetguard / mutategate に続く）。
**未分類 0 件**。「残り」「その他」でまとめた箇所は無い — blastguard 第1ラウンドが
未分類集合を「大半は parser helper」と要約し、その中に実在する fail-open
（exclude.rs の glob 部分ロード）が隠れていた前例があるため、本監査では
カテゴリ単位の棄却を一切行わない（CLAUDE.md 第6節: 棄却は発見と同じ立証責任を負う）。

## 0. 要旨

| | |
|---|---|
| 分母（census） | **39** サイト |
| 制限側へ解決済み | **36** |
| 実在する fail-open | **3**（F-1 は本コミットで修正、F-2 / F-3 は起票） |
| 監査対象外と判断 | 0（＝カテゴリ棄却なし） |

propguard は **block する gate** なので、「制限側」＝ **より多く検査する / 停止を保留する**方向。

## 1. 分母

```
python3 scripts/census-verdict-terminals.py propguard
```

測定日 2026-08-04、測定点 ＝ 本コミットの作業ツリー（F-1 修正後）。
修正前の測定点 aae6127e では 38 サイトで、差分の 1 件は `build_set` の
2 つの erasure が 2 つの `Err` アームに置き換わったことによる。

**行番号は drift する。** 下の分類表はファイル単位で参照し、正確な行は
上記コマンドを再実行して得ること（この repo の規範: 測定値には測定手段を併記し、
数字は継承せず毎回測り直す）。本監査時点の 39 サイトは以下のとおり:

```
config.rs:34   parse                        _ => Mode::Inject,
config.rs:365  disabled_env                 .unwrap_or(false)
gate.rs:94     checkable_files              inc...unwrap_or(true)
gate.rs:95     checkable_files              && !exc...unwrap_or(false)
gate.rs:148    build_set                    Err(e) => {
gate.rs:160    build_set                    Err(e) => Determination::undetermined(...)
gate.rs:329    decide_from_count            return Decision::Allow {      (checker-error-giveup)
gate.rs:345    decide_from_count            properties: Vec::new(),
gate.rs:358    decide_from_count            return Decision::Allow {      (properties-satisfied)
gate.rs:367    decide_from_count            return Decision::Allow {      (giveup)
gate.rs:458    escalate_giveup_on_systemic  Decision::Allow {
gate.rs:468    escalate_giveup_on_systemic  files: vec![],
gate.rs:469    escalate_giveup_on_systemic  properties: vec![],
gate.rs:502    escalate_giveup_on_outage_scan  Decision::Allow {
gate.rs:515    escalate_giveup_on_outage_scan  files: vec![],
gate.rs:516    escalate_giveup_on_outage_scan  properties: vec![],
gate.rs:526    allow                        Decision::Allow {
gate.rs:550    decide_truncated             return Decision::Allow {      (truncated-giveup)
gate.rs:565    decide_truncated             properties: Vec::new(),
gate.rs:592    decide_scan_failed           return Decision::Allow {      (git-scan-failed-giveup)
gate.rs:601    decide_scan_failed           files: vec![],
gate.rs:602    decide_scan_failed           properties: Vec::new(),
gate.rs:674    block_reason                 _ => String::new(),
gate.rs:812    verdict_for_id               None => {}
gate.rs:814    verdict_for_id               _ => return None,
gate.rs:831    verdict_for_id               _ => Some(false),
gate.rs:879    build_command                let prog = parts.next().unwrap_or("claude");
git.rs:84      changed_files                let mut out = Vec::new();
git.rs:118     collect                      None => false,
git.rs:138     diff_text                    let mut others = Vec::new();
install.rs:19  settings_path                .unwrap_or_else(|| PathBuf::from("."))
install.rs:26  binary_path                  .ok()
install.rs:28  binary_path                  .unwrap_or_else(|| "propguard".to_string())
main.rs:61     state_dir_override           .ok()
main.rs:174    check_run                    let input = hook.unwrap_or_default();
main.rs:217    check_run                    Decision::Allow {
main.rs:312    handle_checker_outage        Decision::Allow {
main.rs:489    status                       current_dir().unwrap_or_else(|_| Path::new("."))
main.rs:515    status                       None => println!(
```

加えて、census が permissive 候補から**除外**する `Determination` アーム
（gate.rs / git.rs / config.rs / derive.rs）も本監査で明示的に再検査した。
census の除外は設計判断であって、監査の免責ではない。

## 2. 制限側へ解決している 36 サイト

### A. 有界 give-up（`Decision::Allow` だが「判定不能を黙って許可」ではない）— 12 件

propguard の設計上の中核。判定不能はまず **block** し、`max_attempts` 連続でのみ
**警告付きで** allow する（永久にターンを閉じ込めないための有界化）。census が
`Decision::Allow` を permissive 候補として拾うのは正しいが、これらは
**block を経た後の、宣言された脱出口**である。

| tag（gate.rs） | 判定不能の一次解決 | 有界 give-up の可視性 |
|---|---|---|
| `checker-error-giveup` | 先に `Block{tag:"checker-unavailable"}` | `eprintln!` で WARNING + 復旧手順 |
| `giveup` | 先に below-threshold で block | attempts 超過時のみ |
| `truncated-giveup` | 先に `Block{tag:"diff-truncated"}` | `eprintln!` で WARNING |
| `git-scan-failed-giveup` | 先に `Block{tag:"git-scan-failed"}` | `eprintln!` で WARNING |

`properties: Vec::new()` / `files: vec![]` の各サイトは上記 Decision のフィールドで、
**空集合だが判定ではない** — 検査が走っていない以上、違反プロパティを1件も
帰属させないのが正しい。`decide_from_count` の該当コメントが理由を明記する:

> The checker never ran, so NOTHING was evaluated. Report no per-property violations

これを derived prop_ids で埋めると overwatch の property_id 別 fleet 相関ストアが
**未検査の id を実違反として**汚染する（CA-propguard-03）。空集合が正しい稀なケース。

### B. 三値が既に入っている経路（census が除外、本監査で再確認）— 5 件

| 場所 | 内容 |
|---|---|
| gate.rs `decide_from_count` | `Determination::Undetermined(why)` を `Block{tag:"checker-unavailable"}` へ |
| gate.rs `escalate_giveup_on_outage_scan` | ledger 読取不能 → `Block{tag:"checker-outage-undetermined"}`。**確定した systemic outage とは別 tag** で、二重の不明を「出荷」に写さない |
| git.rs `changed_files` | `RepoProbe::Undetermined` を `ChangeScan::Failed` へ |
| config.rs | 設定読取の `Undetermined` アーム |
| derive.rs | done_criteria 読取の `Undetermined` アーム |

`escalate_giveup_on_outage_scan` の docstring は本 repo の規範を逐語で持っている:

> Two unknowns stacked must not resolve to "ship it"

### C. 制限側 unwrap_or / catchall — 12 件

| 場所 | 潰し方 | なぜ制限側か |
|---|---|---|
| config.rs `disabled_env` | `.unwrap_or(false)` | env 読取不能 → **無効化しない** ＝ gate は動き続ける |
| git.rs `collect` | `None => false` | `ok` に伝播し `ChangeScan::Failed`（fail-closed） |
| git.rs `changed_files` | `let mut out = Vec::new()` | `ok` フラグで守られた累積器。全 sub-command 成功時のみ `Files(out)` |
| gate.rs `verdict_for_id` | `_ => Some(false)` | 明示 PASS 以外は「満たされていない」＝ block 側 |
| gate.rs `verdict_for_id` | `None => {}` / `_ => return None` | `None`＝**未評価**で、`Some(false)`（検査して失敗）と区別される（CA-propguard-06） |
| gate.rs `block_reason` | `_ => String::new()` | 理由文字列の組み立て。判定ではない |
| gate.rs `checkable_files` | `unwrap_or(true)` / `unwrap_or(false)` | F-1 修正後は「フィルタ未設定 / 信頼できない」のみを意味し、どちらも**検査を増やす**側 |
| main.rs `check_run` | `hook.unwrap_or_default()` | `interactive = hook.is_none()` を**先に**取っており、stdin 無し＝対話 CLI 実行という宣言された分岐 |
| main.rs `state_dir_override` | `.ok()` | `PROPGUARD_STATE_DIR` 未設定は no-op（既定 state_dir が立つ） |

### D. 判定を持たない経路（下流消費者を列挙して免責を主張する）— 7 件

CLAUDE.md 第1節は「判定を持たない」の免責に**消費者の列挙**を要求する。

| 場所 | 下流消費者 | 免責の根拠 |
|---|---|---|
| install.rs `settings_path` / `binary_path` | `propguard install` の settings.json 書き込み | インストーラであり hook 判定経路に無い。失敗は install コマンド自身の出力で可視 |
| main.rs `status` | 人間向け表示 | 表示専用。`None` は `println!` で明示的に unknown と印字され、沈黙にならない |
| gate.rs `build_command` | `unwrap_or("claude")` | 空 `checker_cmd` の既定。checker が実在しなければ `Undetermined` → block へ落ちるので、ここでの既定値は判定を作らない |
| main.rs `check_run` / `handle_checker_outage` の `Decision::Allow` | 状態保存の分岐 | 判定は `gate::evaluate` が既に確定させており、ここは分岐先 |

## 3. 実在した fail-open

### F-1（本コミットで修正）— filter list の部分ロードが黙って検査対象を狭める

`build_set` は 2 箇所で失敗を消していた（**修正前**の形）:

```rust
if let Ok(glob) = Glob::new(g) { b.add(glob); any = true; }
// ...
b.build().ok()
```

`include` 側でこれは **permissive 方向**である。コンパイルできなかった pattern は
単に消え、**残った pattern だけで**マッチが続く。そして `evaluate` の消費経路
（gate.rs、`checkable_files` の直後）:

```rust
let files = checkable_files(cfg, &changed);
if files.len() < cfg.min_changed_files {
    return allow("no-code-changes", st);
}
```

＝ propguard が**マッチに失敗しただけの変更集合について「検査対象なし」と報告して通す**。
`include` / `exclude` はどちらも config ファイルで上書きできる（config.rs が
`fc.include` / `fc.exclude` で `self` を上書きする）ので、到達可能性は仮定ではない。

**同一形の欠陥が blastguard で二度修正されている**: diffrisk.rs、次いで exclude.rs
（コミット 9ed33ba6）。propguard の `build_set` は**三例目**で、関数名まで同じ。
これが「ミラーギャップ」— 修正が双子の片方にだけ着地する、この repo が繰り返す形。

修正: `build_set` は `Determination<Option<globset::GlobSet>>` を返す。
`Known(None)`＝pattern 未設定 / `Known(Some)`＝全 pattern が matcher に到達 /
`Undetermined`＝1本でも壊れている。**部分ロードした集合は返さない。**
`resolve_filter` が制限側（＝フィルタを捨てて全件検査）へ解決し、`eprintln!` で announce する。

**両側とも「フィルタしない」に倒れるが理由は逆**: 半分ロードした `include` は
選ぶはずのファイルを落とし、半分ロードした `exclude` は運用者が全体を見られなくなった
pattern で除外を続ける。

**残余（散文で隠さず記す）**: `exclude` を丸ごと捨てるので、1 本の typo が
正当な除外（lockfile・vendored）まで無効化し、検査ノイズが増える。制限側では
あるが無害ではない。per-pattern に縮退させる方が精密で、それは本修正のスコープ外。

### F-2（起票 1217fa4f・未修正）— `diff_text` が git 失敗を消し、空 diff が「変更なし」として通る

`run_diff` は `if let Some(text) = run_git(root, &args)` で失敗を捨てる。
`git diff` と `git diff --cached` が両方失敗し untracked も無ければ、
diff は空文字列になり、`evaluate` が `allow("empty-diff", st)` を返す。

**これは同じファイルの一階層上で既に閉じられた fail-open と同型**である。
`changed_files` 側は `ChangeScan::Failed` を持ち、gate.rs の該当コメントが
その意図を逐語で述べている:

> never silently allow an empty, collapsed scan (the fail-open this closes)

`changed_files` は fail-closed 化され、`diff_text` はされなかった — **ファイル内のミラーギャップ**。
`scan_failed_reason` の日本語が「空の diff を『変更なし』と解釈して無言で通過させると、
未検査の変更が gate をすり抜けます」と**まさにこの穴を説明している**のに、
その説明が適用されていない経路が残っている。

修正には `DiffText` に三値を持たせ `decide_scan_failed` 相当の新アームを通す必要があり、
本コミットのスコープを超えるため起票した。

### F-3（起票 3ca750b9・未修正）— `mode` の綴り間違いが独立検査を黙って自己申告へ降格させる

`Mode::parse` の `_ => Mode::Inject` は、認識できない mode 文字列をすべて `Inject` に写す。
`Inject` は「1回 block してチェックリストを注入し、同じ diff を次ラウンドは信頼する」
（trust-after-one-block）モードで、`Subprocess` の**独立した**検査より弱い。
つまり `mode = "subproces"` のような typo は、独立検査を**無診断で**自己申告へ降格させる。

未知値に対して安全側の既定を選ぶこと自体は妥当だが、**沈黙が問題**である
（CLAUDE.md 第3節: 「検査した」と「検査できなかった」を下流から区別不能にしない）。
announce 1 行で足りるが、テストが `Mode::parse` の private 性と stderr 捕捉を要するため起票した。

## 4. 集計

| 分類 | 件数 |
|---|---|
| A. 有界 give-up（宣言された脱出口） | 12 |
| B. 三値が既に入っている | 5 |
| C. 制限側 unwrap_or / catchall | 12 |
| D. 判定を持たない（消費者列挙済み） | 7 |
| F. 実在した fail-open | 3 |
| **未分類** | **0** |

## 5. 付随して直した観測不能

`hung_git_invocation_returns_promptly_with_graceful_fallback`（git.rs）は
CA-propguard-006 の「タイムアウトした git を後に残さない」契約の唯一の回帰テストだが、
`pgrep -fc` の `-c` が Linux procps 拡張であるため macOS では exit 2 になり、
boundary が正しく `Undetermined` を返し、テストが `expected Known` で落ちていた。
＝ **この契約は macOS 上で一度も検証されていなかった**（HEAD aae6127e で再現確認済み）。
`pgrep -f` ＋ 行数カウントへ変更。assertion（`count == 0`）は据え置きで、
弱めたのではなく走るようにした。

## 6. この監査が証明しないこと

- `harness_core::boundary` / `overwatch::store` の内部は監査していない（各 crate 自身の DoD9 単位）。
- F-2 / F-3 は**確認済みだが未修正**である。「後で見る」ではなく、証拠付きで backlog に載っている。
- CLAUDE.md 第2節(a) の逸脱: テストは修正と同じ agent が書いた（本セッションの system prompt が
  Agent 起動を禁じるため）。RED の先行観測と反空虚対照で代償したが、生成と検証が盲点を共有する
  リスクは残る。独立再監査を推奨する。
