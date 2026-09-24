# gauge の verdict 経路 監査（read-only、逐語引用つき）

- **対象**: `crates/gauge/src/`（`config.rs` / `install.rs` / `main.rs` / `model.rs` / `report.rs` /
  `store.rs` / `window.rs` — src 直下 7 ファイル・計 1556 行）。`model.rs` と `store.rs` は
  `pub use` のみのシムなので、実体は 5 ファイル。
- **監査日**: 2026-09-24
- **監査時点**: worktree `/Users/yuki/src/.harness-worktrees/b8-gauge-0f876834`、branch `b8/gauge-0f876834`、
  HEAD `b47f08e3`
- **対象バージョン**: gauge 0.3.14（`crates/gauge/Cargo.toml:4`）/ harness-core 0.2.27
- **性質**: **read-only**。`crates/gauge/` 配下のコード・テストは 1 行も変更していない。
  ここに記す P（permissive）項目は**すべて未是正**である。「見つけたが直していない」ことを明記する。
- **位置づけ**: `docs/audit-blastguard-verdict-paths.md` / `docs/audit-budgetguard-verdict-paths.md` /
  `docs/audit-reviewgate-verdict-paths.md` と同じフォーマット（P/R/D の 3 分類・逐語引用・実測・
  棄却にも同じ立証責任）に従う、compass charter DoD9（各 gate crate の verdict 経路の per-gate 監査）の 1 本。
  backlog `9236` 行の item（「gauge crate の DoD9 verdict 経路監査」）に対応する。

分類（テンプレート踏襲。3-way の P/R/D は Producer/Router/Decider ではなく
permissive/restrictive/deliberate の意。各項目には併せて **産出(P-site) / 中継(R-site) / 判定(D-site)**
の役割も付記する）:

- **P（permissive）** — 判定不能・IO 失敗・parse 不能・空集合を「問題なし」へ潰している。**未是正**。
- **R（restrictive）** — 判定不能を制限側へ解決している。見た目が permissive でも**変更してはならない**。
- **D（deliberate）** — 意図的に許可へ解決する**宣言済みの仕様**。permissive とは別物。

---

## 0. 結論を先に — gauge は「判定を持つ」crate である（observability 免責は成立しない）

CLAUDE.md 第1節は **「判定を持つ」は返り値の型ではなく消費のされ方で決まる**と定め、免責を主張する
モジュールに**下流消費者の列挙**を要求している。gauge の module header は自分を観測専用と宣言する:

```rust
// crates/gauge/src/main.rs:8-11
//! Like the rest of the toolkit the hook can only *observe*: `record` runs
//! under `run_hook`, which catches panics and always exits 0, so bad/empty
//! stdin, a missing transcript, or an unwritable store all record nothing
//! rather than break the turn.
```

「**rather than break the turn**」は CLAUDE.md 第1節が
「**この語を verdict 経路の docstring・コメント・コミットメッセージに書いた時点で赤信号**」と
名指ししている当のものである。したがって列挙義務が発生する。**列挙した結果、免責は成立しない**:

| # | 下流消費者 | 何を読むか | 何になるか |
|---|---|---|---|
| C1 | `crates/budgetguard/src/gate.rs:304` `if let Some(rec) = session::load_one(gauge_state_dir, session_id) {` | gauge が書いた `SessionRecord.models` | `pricing::session_cost` → `verdict()` → **`{"decision":"block"}`（ユーザのターンを実際に止める）** |
| C2 | `crates/budgetguard/src/gate.rs:184` `let rec = session::load_one(gauge_state_dir, session_id)?;` | 同 record の `models` | cache-health の判定（`additionalContext` として注入） |
| C3 | `crates/harness-status/src/sessions.rs:43` `let mut records = match session::load_all(&session::default_state_dir()) {` | store 全体 | `/status` パネル |
| C4 | `crates/session-insights/src/record.rs:80,114` `session::load_one(&session::default_state_dir(), ctx.session_id)` | record の `models` / `agents` | Obsidian record ノートの `## コスト` ブロック（人間が「このセッションはいくらかかったか」として読む） |
| C5 | `crates/condukt/src/state.rs:2248` `let out = std::process::Command::new("gauge")` | gauge の stdout | task ごとの実コスト → `record-run --cost` → **fugu-router の routing 方策（どのモデルを使うか）** |
| C6 | `crates/condukt/src/state.rs:2281` `let out = std::process::Command::new("gauge")` | 同上（`tokens_input`/`tokens_output`） | 同上 |
| C7 | `crates/condukt/skills/condukt/SKILL.md:1006` `AGENT_ID=$(gauge subagents --json ${SID:+--session "$SID"} 2>/dev/null` | stdout | worker の transcript 特定 |
| C8 | condukt SKILL.md の `record-run` フォールバック（逐語は下の箇条書き） | stdout | task の記録コスト |
| C9 | 人間 | `gauge report` / `gauge status` / `gauge session` | 支出の判断 |

表のセルに入れると markdown の `|` エスケープで逐語性が壊れるものを、表の外に置く:

- **C3 の自己宣言** — `crates/harness-status/src/sessions.rs:32` 「numbers are read by a human as spend.」。
  harness-status 自身が、このパネルの数字は人間に支出として読まれる＝判定を持つ側だと書いている。
- **C8** — `crates/condukt/skills/condukt/SKILL.md:1105` `GAUGE_COST=$(gauge session --json ${SID:+--session "$SID"} 2>/dev/null | jq -r '.cost_usd // empty' 2>/dev/null || true)`。
  末尾の `|| true` が gauge の exit code を消すので、gauge 側が非 0 で「unknown」を主張しても
  この消費者には届かない。

**C1 は allow/block を実際に返すゲートである。** したがって gauge は observability crate ではなく、
**judging crate（判定を持つ crate）**である。以下の監査はその前提で行う。

ただし精密に述べる: **`run_hook` の exit-0 panic barrier そのものは R である**。record が
**書かれなかった**場合、C1 は `load_one` の `None` から
`estimate_transcript_cost` による transcript 全再パースへ fallback する（`crates/budgetguard/src/gate.rs:310-316`）——
これは正確な（過少計上しない）情報源である。
**救われないのは「record が書かれなかった」ではなく「record が *部分的に* 書かれた」場合**であり、
それが下の P1 である。**欠落した record と過少計上された record は、下流から見て別物**である。

---

## 1. 方法 — 分母は機械が出す

読んで数えた「全部見た」は観測ではなく予測である（CLAUDE.md 第2節）。分母は script が出し、
分類だけを人間（LLM）が行う。

```
python3 scripts/census-verdict-terminals.py gauge
```

**測定値（測定日 2026-09-24、測定点 `b47f08e3`）**:

```
=== gauge: 45 production verdict-terminal sites (0 in tests) ===
config.rs          4 sites  {"empty_collection": 1, "unwrap_or": 3}
install.rs         3 sites  {"ok_erase": 1, "unwrap_or_else": 2}
main.rs           32 sites  {"determination": 8, "err_arm": 6, "none_arm": 3, "ok_erase": 4, "unwrap_or": 5, "unwrap_or_else": 6}
report.rs          3 sites  {"unwrap_or": 3}
window.rs          3 sites  {"ok_erase": 3}
=== permissive-or-collapsing terminals to classify: 37 ===
```

**census は網羅していない**（これ自体が所見である）: `crates/gauge/src/config.rs:83-84` の
`if let Ok(text) = … { if let Ok(fc) = … { … } }`（else の無い二重ネスト）は
37 件のどこにも現れない。**P3 はそこにある。**

**もう 1 つの分母**: `python3 scripts/check-fail-open.py --all` は 2026-09-24 / `b47f08e3` 時点で
**gauge に 1 件もヒットしない**。それでも以下の 14 件が出る。
blastguard §3.3 の教訓（カテゴリでの却下は個別の検証ではない）がそのまま当てはまる:
**スキャナが黙ったことは、経路が無いことの証拠ではない。**

### 1.1 実測（判断ではなく観測。すべて本監査で自分で観測した）

リリースビルド（`target/release/gauge` 0.3.14 / `target/release/budgetguard` 0.1.20）を
ブラックボックスで実行。probe スクリプトは
`/private/tmp/claude-502/-Users-yuki-src-harness/2444be10-e07a-4c9a-a785-01f58d3995fa/scratchpad/probe{,2,3,4}.sh`。

| Probe | 注入 | 観測結果 | 対応項目 |
|---|---|---|---|
| **4-1** | 対照: 全部 readable。main 1k/100 tok + sub-agent 1 本 9M/9M opus | budgetguard `{"decision":"block"}` / `session $270.0075` | 対照（アンチ空虚） |
| **4-2** | `agent-big.jsonl` **1 ファイルだけ** `chmod 000`（dir は readable） | budgetguard **block されない**・`session $0.0075`・stdout/stderr に診断ゼロ。gauge が persist した record の `agents` は `main` のみ（$270 の子が消滅） | **P1** |
| **4-3** | 対照: `subagents/` **dir ごと** `chmod 000` | budgetguard `{"decision":"block"}` 「未計測の支出は $0 ではありません」 | R1（dir 粒度の guard は存在し、効いている） |
| **2-1/2-3** | 対照: model id = `claude-opus-4-8` / `claude-sonnet-4-5`、1M in / 1M out | `block` at $30.0000 / $18.0000 | 対照 |
| **2-2** | model id を `claude-newfamily-1` に（トークン数は同一） | `session $0.0000`・**block されない** | **P2** |
| **B0/B1/B2** | record を `chmod 000` / 中身を `not json` に | `gauge session --json --session <id>` → **`null` + exit 0**（対照 B0 は正しい JSON + exit 0） | **P6** |
| **B3** | 壊れた record が 1 件だけ | `gauge report` → **「no sessions recorded yet.」+ exit 0**（record は存在する） | **P5** |
| **B3b** | 壊れた record ＋ 正常 record | `gauge report` → **「1 セッション / 合計 $5.00」**（2 件分の支出のうち 1 件が無音で消える） | **P5** |
| **B4** | 対照: `sessions/` dir ごと `chmod 000` | `gauge: session store unknown …` + **exit 1** | R2（dir 粒度の guard は効いている） |
| **C0/C1** | `gauge.toml` の `input` を `inputt` に（1 文字） | 対照 $5.00 → **$0.0000**。`gauge status` は `pricing: 1 override(s) + built-in` と健全そうに表示 | **P4** |
| **D1** | `gauge.toml` に TOML 構文エラー 1 個（10 倍の price override ＋ 独自 `state_dir` を宣言した状態で） | ファイル全体が無音で破棄。`gauge status` は `config: …/gauge.toml` と**採用済みのように**表示しつつ `pricing: built-in` | **P3** |
| **E** | 3 セッション（opus / opus-5 / `some-new-frontier-model`）各 1M input | `some-new-frontier-model $0.0000  in 1.00M`、合計 `$10.00`（正しくは $15.00） | **P2** |
| **F0/F1** | `last_ts` が `null` の record ＋ 正常 record | `--since` 無し → 合計 $10.00 / `--since 2026-01-01` → **合計 $5.00** | **P9** |
| **A0/A1/A2** | `~/.claude/projects` を `chmod 000` / `HOME` 未設定 | `gauge subagents --json` → **`[]` + exit 0**（A1 は stderr に warning あり、A2 は**診断ゼロ**） | **P7** |
| **R0/R1** | 対照: `subagents/` dir ごと `chmod 000` | `gauge record` が「not recording session …: the canonical record must not hold an under-count」で書き込み拒否。`gauge subagents --json` → `{"status":"unknown",…}` + **exit 1** | R1/R4（この guard は正しく効いている） |
| **R2b** | `subagents/` 内の **1 ファイルだけ** `chmod 000` | `gauge subagents --json` → そのエントリが**配列から消えるだけ**・exit 0・診断ゼロ | **P1** |

**`cargo test -p gauge` は 26 件すべて green**（unit 21 + 統合 5）。
**$270 の sub-agent が正典 record から消えている状態で、である。** blastguard §3.3 の
「366 本が全て green だった」と同型。

---

## 2. P — 見つかった permissive 経路（14 件。**すべて未是正**）

### P1. 個別 sub-agent transcript **ファイル**の read 失敗が無音で落ち、正典 record に過少計上を焼き込む【CONFIRMED / CRITICAL】

役割: **産出(P-site)**（`record_hook` が canonical record を書く）＋**中継(R-site)**（`subagents --json`）。

gauge の module header は、これを**しないこと**を明示的な契約として宣言している:

```rust
// crates/gauge/src/main.rs:16-18
//! * `record` refuses to persist an aggregate whose sub-agent spend could not
//!   be read (an under-count would be baked into the canonical `SessionRecord`
//!   that budgetguard's gate then trusts), and says so on stderr.
```

実装もそう見える:

```rust
// crates/gauge/src/main.rs:207-223
    // An aggregate whose sub-agent spend could not be read is an under-count of
    // unknown size. Persisting it would write that under-count into the
    // canonical record budgetguard's gate prefers as its cost source
    // (crates/budgetguard/src/gate.rs:164), where it would look like a measured
    // number forever. Record nothing instead, and say why.
    let aggregate = match est.aggregate.complete() {
        Determination::Known(aggregate) => aggregate,
        Determination::Undetermined(why) => {
            eprintln!(
                "gauge: not recording session {}: its cost could not be fully \
                 determined ({why}); the canonical record must not hold an \
                 under-count",
                input.session_id
            );
            return;
        }
    };
```

**この guard は `subagent_scan` しか見ておらず、`subagent_scan` は *ディレクトリ*の失敗しか記録しない。**
ファイルの失敗は 1 行上で消える:

```rust
// crates/harness-core/src/usage.rs:188-203  aggregate()
    if !saw_inline_sub {
        match subagent_files(path) {
            Determination::Known(files) => {
                for file in files {
                    if let Ok(sub_text) = std::fs::read_to_string(&file) {
                        ingest(&mut agg, &sub_text, Some(AGENT_SUB), false, false);
                    }
                }
            }
            Determination::Undetermined(why) => {
                // The sub-agent spend for this session could not be read: these
                // totals are an under-count. Say so instead of returning them
                // (or an empty `None`) as if they were the whole session.
                agg.subagent_scan = Determination::Undetermined(why);
                return Some(agg);
            }
        }
    }
```

`if let Ok(sub_text) = …` に **else が無い**。`Err` は `subagent_scan` を動かさない。
**三値の guard（下半分）と、その guard を無効化する二値の erasure（上半分）が、同じ `match` の中で
隣り合っている。** これは blastguard §3.3 と同じ mirror gap である。

同一の erasure が `subagent_usage`（`gauge subagents --json` の産出元）にもある:

```rust
// crates/harness-core/src/usage.rs:411-414  subagent_usage()
        for file in files {
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
```

`crates/harness-core/src/usage.rs:396-400` の docstring はこれを**宣言済みの仕様**として書いている
（"Only a file that could not even be *read* … is skipped"）。しかし宣言は
`crates/gauge/src/main.rs:16-18` の契約と**矛盾**しており、下流の判定への到達経路を検証していない。

#### 実測（Probe 4。対照 2 本つき）

main transcript は 1,000 in / 100 out（$0.0075）、sub-agent 1 本が 9M in / 9M out opus（$270.00）。
budgetguard は `[session] block_usd = 1.0`。

| case | 注入 | budgetguard の出力 |
|---|---|---|
| 1（対照） | 無し | `budgetguard: session $270.0075 / day $270.0075` → `{"decision":"block","reason":"…セッション予算超過 $270.0075 / $1.00 (上限)。"}` |
| 2（注入） | `agent-big.jsonl` **1 ファイル**を `chmod 000` | `budgetguard: session $0.0075 / day $0.0075` → **判定出力なし＝allow**。stdout にも stderr にも診断ゼロ |
| 3（対照） | `subagents/` **dir** を `chmod 000` | `{"decision":"block","reason":"…このセッションの費用を測定できませんでした（… Permission denied …）。未計測の支出は $0 ではありません。"}` |

case 2 で gauge が persist した record（逐語）:

```json
{ "session_id": "sess-x", … "models": { "claude-opus-4-8": { "input": 1000, "output": 100, … } },
  "turns": 1, … "agents": { "main": { "models": { "claude-opus-4-8": { "input": 1000, … } }, "turns": 1 } } }
```

**`agents` に `sub-agent` バケットが存在しない。** $270 の子は「読めなかった」ではなく
「**居なかった**」として記録されている。`subagent_scan` は `Known(())` のままなので
`complete()` は `Known` を返し、`crates/gauge/src/main.rs:212` `let aggregate = match est.aggregate.complete() {` の
guard は素通りする。

**case 3 が対照として決定的**である。同じセッション・同じ金額で、失敗の粒度だけが
dir か file かで違う。**dir なら block、file なら無音の allow。**
「経路を辿れなかった」ではなく、**両側を実行して差分を観測した**。

#### 波及（この 1 件が他 crate の docstring を偽にしている）

```rust
// crates/session-insights/src/record.rs:97-100
/// `None` also covers "the numbers came from gauge's canonical record" — that
/// record is only ever written from a complete aggregate
/// (`crates/gauge/src/main.rs` `record_hook` refuses an undetermined one), so
/// there is no under-count to disclose on that path.
```

**この文は Probe 4 case 2 によって反証された。** record は過少計上を保持しうる。
その結果 session-insights は `## コスト` ブロックに
`⚠ サブエージェント費用を読めませんでした（以下は過少計上）` を**付けずに**、
過少計上された数字を測定値として提示する（`crates/session-insights/src/record.rs:129-133`）。
CLAUDE.md 第4節（docstring が実挙動と食い違うのは次のレビュアーを騙す仕掛け）に該当する。

#### 是正の方向（この監査では実施しない）

`Aggregate` 側にファイル単位の失敗を集める。最小の形は
`aggregate()` のループで `Err` を捕らえて `agg.subagent_scan = Determination::undetermined(…)` に
倒すこと（dir 側と同じ解決先）。`subagent_usage` 側は `Determination<Vec<SubAgentUsage>>` を
既に返しているので、1 本でも読めなければ `Undetermined` へ倒せば
`gauge subagents` の exit 1 経路（R4）がそのまま効く。
**「1 本読めなくても残りは返す」を維持したいなら、返り値に「不完全である」という channel を足す
ほかない** — 今の `Known(Vec<…>)` は「これが全部だ」としか言えない。

---

### P2. 未知の model id が **$0** に価格付けされ、予算ゲートが恒久的に無効化される【CONFIRMED / HIGH】

役割: **産出(P-site)**（harness-core の価格表）＋ gauge の全 read command が**判定(D-site)**。

```rust
// crates/harness-core/src/pricing.rs:37-65  builtin_rate()
fn builtin_rate(model: &str) -> Rate {
    let m = model.to_lowercase();
    if m.contains("fable") || m.contains("mythos") { … }
    else if m.contains("opus") { … }
    else if m.contains("sonnet") { … }
    else if m.contains("haiku") { … }
    else {
        Rate {
            input: 0.0,
            output: 0.0,
        }
    }
}
```

`else` 節が **`Rate{0.0, 0.0}`** ＝「このモデルの支出は 0」。これは
「価格が分からない」を「価格が 0 である」に写している。gauge の消費サイトは 8 箇所:
`crates/gauge/src/main.rs:272`, `:297`, `:317`, `:520`, `:616` と
`crates/gauge/src/report.rs:72`, `:109`, `:124`。

**そして `.0` が budgetguard の block 判定に直行する**（§0 の C1）。

#### 実測（Probe 2。トークン数は 3 ケースとも 1M in / 1M out で同一、`block_usd = 1.0`）

```
--- CONTROL known model (opus) (model=claude-opus-4-8) ---
budgetguard: session $30.0000 / day $30.0000
{"decision":"block","reason":"budgetguard: セッション予算超過 $30.0000 / $1.00 (上限)。…"}

--- INJECTED unrecognized model (model=claude-newfamily-1) ---
budgetguard: session $0.0000 / day $30.0000
{"additionalContext":"budgetguard: cache hit rate 0.00% …"}      ← 判定なし＝allow

--- CONTROL2 known model (sonnet) (model=claude-sonnet-4-5) ---
budgetguard: session $18.0000 / day $48.0000
{"decision":"block","reason":"budgetguard: セッション予算超過 $18.0000 / $1.00 (上限)。…"}
```

そして gauge 側（Probe E）:

```
モデル別
  claude-opus-4-8              $5.00  in 1.00M / out 0 / cache r 0 w 0 (0% hit)
  claude-opus-5                $5.00  in 1.00M / out 0 / cache r 0 w 0 (0% hit)
  some-new-frontier-model    $0.0000  in 1.00M / out 0 / cache r 0 w 0 (0% hit)
合計コスト $10.00        ← 正しくは $15.00
```

**「1.00M トークン使った」と「$0.0000」を同じ行に並べている。** これは
「測っていない」ではなく「測って 0 だった」という主張である。

#### fail-open が**仕様としてテストに固定されている**

```rust
// crates/harness-core/src/pricing.rs:155-162
    #[test]
    fn unknown_model_is_free() {
        let u = ModelUsage {
            input: 1_000_000,
            ..Default::default()
        };
        assert_eq!(cost("some-local-model", &u, &[]), 0.0);
    }
```

CLAUDE.md 第2節が引く `assert!(checks_verdict(&[]))` と**完全に同型**である。
テスト名が "is_free" と断言しており、**「未知だから測れなかった」という第三の答えを
型もテストも持っていない**。同じ主張は README とモジュール header にもある
（`crates/harness-core/src/pricing.rs:11` "An unrecognized model contributes 0 (so an unknown id never
invents cost)."、`crates/gauge/README.md` "An unrecognized model contributes 0."）。

**「invents cost しない」は片側の懸念しか見ていない。** 反対側（**gate を無効化する**）は
同じ文の中で検討されていない。そして model id は Anthropic 側が変える外部入力である
（`claude-opus-5` が効くのは偶然 `opus` を含むからにすぎない）。

#### 是正の方向

`rate_for` を `Determination<Rate>` にし、gauge の read command は `unknown` を表示、
budgetguard の gate は `undetermined_verdict` へ倒す（budgetguard 側の受け口は既にある）。
**なお local model 等、本当に $0 のケースを潰さないため、`[[pricing]] pattern = "…" input = 0.0` の
明示宣言は `Known(0.0)` として残すこと**（budgetguard の「不在 config は Known default」と同じ線引き）。

---

### P3. `gauge.toml` の read / parse 失敗が「未設定」に写り、無診断で既定値に落ちる【CONFIRMED / HIGH】

役割: **産出(P-site)**（設定）。budgetguard P1/P2 と同型で、**gauge 側は未是正**。

```rust
// crates/gauge/src/config.rs:82-112
        if let Some(path) = chosen {
            if let Ok(text) = std::fs::read_to_string(&path) {
                if let Ok(fc) = toml::from_str::<FileConfig>(&text) {
                    if let Some(v) = fc.enabled {
                        cfg.enabled = v;
                    }
                    …
                }
            }
        }
        cfg
```

`if let Ok(text)` にも `if let Ok(fc)` にも **else が無い**。`Config::load` の返り値は
二値の `Self` であり、「読んでこう設定されていた」と「読めなかった」が同型同値に写る。
budgetguard 監査 §0 が「二値の形が原因である」と名指しした構造そのもので、
**budgetguard は `Determination<Config>` へ是正済み、gauge は当時のまま**である。

行き先の既定値（`crates/gauge/src/config.rs:51-60`）は
`enabled: true / track_tools: true / state_dir: ~/.gauge/store / pricing: Vec::new()` で、
**`pricing: Vec::new()` は「価格 override なし ＝ 内蔵表を使う」**を意味する。

#### 実測（Probe D。10 倍の price override と独自 `state_dir` を宣言した `gauge.toml` に構文エラー 1 個）

```
--- D1 injected (unterminated string) ---
合計コスト $5.00            ← override が消え、内蔵 opus 5/1M に戻っている
EXIT=0

config:       /private/tmp/…/projD/gauge.toml     ← 採用したかのように表示
enabled:      true
state_dir:    /tmp/…/hd/.gauge/store              ← 宣言した state_dir は無視されている
pricing:      built-in                            ← override も無視されている
STATUS_EXIT=0
```

**stdout / stderr のどこにも診断は出ない。** `gauge status` は
「この config ファイルを使っています」と表示しながら、そのファイルの中身を 1 つも
適用していない。reviewgate P7 / §4（壊れた config を採用済みのように表示する）と同一のクラス。

`state_dir` が無音で無視される点はさらに悪い: 別ディレクトリへ記録する意図が黙って取り消され、
**operator が見ているつもりの store と gauge が書く store が乖離する**。

---

### P4. pricing override の rate 欠落が `unwrap_or(0.0)` で **$0** になり、内蔵表を上書きして勝つ【CONFIRMED / HIGH】

役割: **産出(P-site)**。

```rust
// crates/gauge/src/config.rs:94-109
                    if let Some(rows) = fc.pricing {
                        cfg.pricing = rows
                            .into_iter()
                            .filter_map(|r| {
                                let pattern = r.pattern?.trim().to_lowercase();
                                if pattern.is_empty() {
                                    return None;
                                }
                                Some(PriceOverride {
                                    pattern,
                                    input: r.input.unwrap_or(0.0),
                                    output: r.output.unwrap_or(0.0),
                                })
                            })
                            .collect();
                    }
```

`pattern` は `?` で**欠落を拒否**しているのに、`input` / `output` は `unwrap_or(0.0)` で
**欠落を「無料」として受理**する。そして `rate_for` は override を内蔵表より**先に**見る
（`crates/harness-core/src/pricing.rs:69-80`）ので、**不完全な override が正しい内蔵価格を上書きして勝つ**。

CLAUDE.md 第3節「`unwrap_or` の既定値は**必ず制限側**」に真っ向から反する。
コスト計上における制限側は「高い方」か「未確定」であって、0 ではない。

#### 実測（Probe C。`input` を `inputt` に — 1 文字のタイポ）

```
--- C0 control (no gauge.toml) ---
合計コスト $5.00

--- C1 injected (typo inputt) ---
合計コスト $0.0000  ·  トークン 1.00M (1,000,000)
…
pricing:      1 override(s) + built-in       ← 健全そのものに見える
STATUS_EXIT=0
```

**タイポ 1 文字で、そのモデルの全履歴が $0 に再価格付けされる**（gauge は
「Cost is recomputed from stored token counts on every report」なので過去分もすべて）。
budgetguard 監査 §0 の「タイポ 1 個でゲートが黙って無効化される」と同型。

**対比**: budgetguard 側の同じ設定は `input` / `output` が**非 Option の必須フィールド**で
（`crates/budgetguard/src/config.rs:50` 近傍の `PriceOverrideCfg`）、欠落すれば
`toml::from_str` が失敗し `Determination::Undetermined` → **fail closed**。
**同じ設定項目が、片方の crate では fail-closed、もう片方では fail-open。** gate-crate audit の
mirror gap そのもの。

---

### P5. 個別 record の read / parse 失敗が `load_all` で無音 skip され、`gauge report` が過少計上する【CONFIRMED / MEDIUM-HIGH】

役割: **中継(R-site)** ＋ gauge 側の**判定(D-site)**。

```rust
// crates/harness-core/src/session.rs:216-225  load_all()
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&path) {
            if let Ok(rec) = serde_json::from_str::<SessionRecord>(&text) {
                out.push(rec);
            }
        }
    }
    Determination::Known(out)
```

**directory 粒度は三値化済みだが、file 粒度は二値のまま**（P1 と同じ mirror gap）。
`Determination::Known(out)` は「これが store の全件だ」という主張である。

gauge 側の消費は 3 箇所で、いずれも**数字を断定する**:

```rust
// crates/gauge/src/main.rs:239   report_cmd
    let mut records = known_records(store::load_all(&cfg.state_dir));
// crates/gauge/src/main.rs:257   session_cmd（最新セッション選択）
        known_records(store::load_all(&cfg.state_dir))
// crates/gauge/src/main.rs:612   status
    let (sessions_line, cost_line) = match store::load_all(&cfg.state_dir) {
```

#### 実測（Probe B3 / B3b。対照 B4 つき）

```
--- B3 壊れた record が 1 件だけ ---
no sessions recorded yet. Run some turns, then `gauge report`.
EXIT=0
```

**record は存在するのに「no sessions recorded yet.」**。`crates/gauge/src/report.rs:78-80` の
「正直な空状態」の枝へ、**偽の空**が合流している。

```
--- B3b 壊れた record ＋ 正常 record ---
gauge — 1 セッション / 3 turns
合計コスト $5.00  ·  トークン 1.00M (1,000,000)
EXIT=0
```

2 件分（$10.00）の支出が $5.00 として報告される。**「1 セッション」という数字が
「store に 1 件しかない」と断定している。**

```
--- B4 control: sessions/ dir ごと chmod 000 ---
gauge: session store unknown — it could not be read, so this report would be blank rather than empty: …
EXIT=1
```

対照 B4 が示すとおり、**dir 粒度の guard（R2）は存在し、正しく効いている**。
欠けているのは file 粒度だけである。

`crates/harness-core/src/session.rs:176-177` の docstring は
「individual `.json` files that are unreadable or unparseable are skipped (one corrupt record must not
hide the rest)」と**宣言**している。宣言は「残りを隠さない」を守るが、
**「隠した」ことを伝える channel を持たない**。CLAUDE.md 第3節の「空集合を返さない」は
部分集合にも同じ理由で当てはまる（reviewgate P2 の「部分取得」と同型）。

---

### P6. `gauge session --json` が record の read / parse 失敗を `null` + exit 0 に潰す【CONFIRMED / MEDIUM】

役割: **中継(R-site)**。

```rust
// crates/gauge/src/main.rs:254-269
    let rec = if let Some(id) = session_id {
        store::load_one(cfg.state_dir.as_path(), id)
    } else {
        known_records(store::load_all(&cfg.state_dir))
            .into_iter()
            .max_by(|a, b| a.updated_at.cmp(&b.updated_at))
    };

    let Some(rec) = rec else {
        if json {
            println!("null");
        } else {
            println!("no sessions recorded yet.");
        }
        return;
    };
```

`--session <ID>` を渡した枝だけ `known_records` を通らない。`load_one` は

```rust
// crates/harness-core/src/session.rs:164-168
pub fn load_one(state_dir: &Path, session_id: &str) -> Option<SessionRecord> {
    let path = sessions_dir(state_dir).join(format!("{}.json", safe_id(session_id)));
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}
```

で **`.ok()?` が read 失敗・parse 失敗・不在をすべて同じ `None`** に写す。

#### 実測（Probe B）

| case | 注入 | 出力 | exit |
|---|---|---|---|
| B0（対照） | なし | `{"agents":{},"cost_usd":5.0,…}` | 0 |
| B1 | record を `chmod 000` | `null` | **0** |
| B2 | record の中身を `not json` に | `null` | **0** |

これは **module header の明文の宣言に反する**:

```rust
// crates/gauge/src/main.rs:19-21
//! * the read commands (`report` / `session` / `status` / `subagents`) print
//!   **`unknown`** and exit non-zero when the store or the sub-agent transcripts
//!   cannot be read. An unreadable store is not "$0.00 across 0 sessions".
```

`session` は列挙されているが、`--session <ID>` 経路は `unknown` を出さず exit 0 する。
**CLAUDE.md 第4節（散文が実装と食い違う）に該当。** 消費者は
`crates/condukt/skills/condukt/SKILL.md:1105` の
`GAUGE_COST=$(gauge session --json ... | jq -r '.cost_usd // empty' ... || true)` で、
`null` は `.cost_usd // empty` から空文字になり、**コスト未記録として静かに流れる**。

---

### P7. `gauge subagents --json` が「transcript を読めなかった」を `[]` + exit 0 に潰す【CONFIRMED / MEDIUM】

役割: **中継(R-site)**。

同じ関数の 20 行下は、**まさにこの出力を出さないため**に exit 1 している:

```rust
// crates/gauge/src/main.rs:498-517
    let subs = match usage::subagent_usage(&path.to_string_lossy()) {
        Determination::Known(subs) => subs,
        // The sub-agent transcripts exist but could not be enumerated. Printing
        // `[]` here would tell condukt's cost attribution
        // (crates/condukt/src/state.rs:1291) "this session had no sub-agent
        // spend", which is a claim gauge did not observe. Fail loudly instead:
        // a non-zero exit makes condukt's soft probe fall back rather than
        // record a fabricated zero.
        Determination::Undetermined(why) => {
            …
            std::process::exit(1);
        }
    };
```

その 8 行**上**が、同じ `[]` を出す:

```rust
// crates/gauge/src/main.rs:490-497
    let Some(path) = find_transcript(session_id) else {
        if json {
            println!("[]");
        } else {
            println!("no transcript found.");
        }
        return;
    };
```

`find_transcript` は `Option` しか返せないので、**「transcript が無い」と
「transcript があるか確かめられなかった」が同じ `None`** に合流する:

```rust
// crates/gauge/src/main.rs:363-376
fn find_transcript(session_id: Option<&str>) -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    let projects = Path::new(&home).join(".claude").join("projects");
    let mut best: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
    let proj_entries = match std::fs::read_dir(&projects) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            eprintln!(
                "warning: find_transcript: cannot read {}: {e}",
                projects.display()
            );
            return None;
        }
    };
```

同ファイル `:356-362` の docstring はこの合流を認識し、「`eprintln!` で surface する」と
書いている。**警告は出るが、判定は変わらない** —「診断を出した」と「判定を制限側へ倒した」は別物である。

#### 実測（Probe A）

| case | 注入 | stdout | exit | 診断 |
|---|---|---|---|---|
| A0（対照） | readable な空 projects dir | `[]` | 0 | なし（正しい） |
| A1 | `~/.claude/projects` を `chmod 000` | `[]` | **0** | stderr に warning |
| A2 | `HOME` 未設定 | `[]` | **0** | **なし**（`var_os("HOME")?` は `?` で黙る） |

A2 は診断すら出ない。`crates/gauge/src/main.rs:19-21` の宣言（`subagents` は unknown を出して非 0 終了する）に反する。

**消費側での実害は限定的**（`crates/condukt/src/state.rs:2231-2237` の `parse_agent_cost` は
空配列でも `None` を返し、caller は手動記録値へ fallback する）が、
**`[]` + exit 0 という出力自体が「この session に sub-agent の支出は無かった」という断定**であり、
CLAUDE.md 第3節の「空集合を返さない」に反する。gauge 自身が 8 行下でそう書いている。

---

### P8. `Path::exists()` が「問いに答えられなかった」を「無い」に写す【UNVERIFIED / MEDIUM】

役割: **産出(P-site)**。budgetguard P3（`try_exists` へ是正済み）と同型で、**gauge は未是正**。

```rust
// crates/gauge/src/config.rs:73-81
        let chosen = {
            let p = Config::project_path(root);
            if p.exists() {
                Some(p)
            } else {
                let h = Config::home_path();
                h.exists().then_some(h)
            }
        };
```

```rust
// crates/gauge/src/config.rs:122-132  config_source()
        let p = Config::project_path(root);
        if p.exists() {
            return p;
        }
        let h = Config::home_path();
        if h.exists() {
            return h;
        }
        PathBuf::from("(defaults — no config file)")
```

`Path::exists()` は**存在しない場合と、権限等で問い自体に答えられなかった場合の両方に `false`** を返す
（budgetguard 監査 P3 の逐語引用と同一の論拠）。結果は P3 と同じ「無音で既定値」。

**UNVERIFIED**: 移植可能な fault injection を構成できなかった。cwd 自身の権限を落とすと
`cd` できず probe が成立しない（Probe H1 として試み、放棄した）。**budgetguard 監査 §7 が
同じ制約を「テスト無し・既知の穴」として記録したのと同じ状態**であり、
CLAUDE.md 第6節に従い **REFUTED ではなく UNVERIFIED として項目を残す**。

---

### P9. `report --since` が `last_ts` を持たない record を合計から落とす【CONFIRMED / LOW-MEDIUM】

役割: **判定(D-site)**。

```rust
// crates/gauge/src/main.rs:244-246
    if let Some(s) = since.as_deref() {
        records.retain(|r| r.day().map(|d| d.as_str() >= s).unwrap_or(false));
    }
```

`r.day()` は `last_ts` が `None` か 10 文字未満なら `None`（`crates/harness-core/src/session.rs:102-106`）。
`unwrap_or(false)` は **「日付が分からない record は除外する」** ＝ 支出を合計から落とす側。
CLAUDE.md 第3節の「`unwrap_or` の既定値は必ず制限側」に反する（ここでの制限側は
「含める」か「不明として別掲」）。

#### 実測（Probe F）

```
--- F0 no --since ---     合計コスト $10.00  ·  トークン 2.00M
--- F1 --since 2026-01-01 ---  合計コスト $5.00  ·  トークン 1.00M
```

`--since` は「2026-01-01 以降」であり、除外された record は
**日付が不明なだけで、期間外だと確定したわけではない**。表示上の区別も一切ない。

---

### P10. 4 つの read command が `current_dir()` 失敗を `"."` に写す【UNVERIFIED / LOW-MEDIUM】

役割: **産出(P-site)**。budgetguard P6（`exit 2` へ是正済み）と同型で、**gauge は未是正**。

```rust
// crates/gauge/src/main.rs:237   report_cmd
    let root = std::env::current_dir().unwrap_or_else(|_| Path::new(".").to_path_buf());
// crates/gauge/src/main.rs:251   session_cmd     （同一行）
// crates/gauge/src/main.rs:488   subagents_cmd   （同一行）
// crates/gauge/src/main.rs:608   status          （同一行）
```

cwd が解決できないときに `"."` へ落ちると、**別プロジェクトの（あるいは存在しない）`gauge.toml` を
読み、別の `state_dir` を報告しうる**。budgetguard 監査 P6 が「別プロジェクトの設定を読む」として
是正した経路と同一。

**UNVERIFIED**: 削除済み cwd の再現が macOS 上で安定しなかった。P8 と同じ扱いで項目を残す。

---

### P11. 直列化失敗が `{}` / `[]` として exit 0 で出力される【UNVERIFIED（latent）/ LOW】

役割: **中継(R-site)**。

```rust
// crates/gauge/src/main.rs:290-293
        println!(
            "{}",
            serde_json::to_string(&out).unwrap_or_else(|_| "{}".to_string())
        );
```

```rust
// crates/gauge/src/main.rs:525-528
        println!(
            "{}",
            serde_json::to_string(&serde_json::Value::Array(arr)).unwrap_or_else(|_| "[]".into())
        );
```

**`crates/gauge/src/main.rs:527` `serde_json::to_string(&serde_json::Value::Array(arr)).unwrap_or_else(|_| "[]".into())` が既定値として出す `[]` は、その 25 行上（P7 で引いた 500-505 行）が
「condukt にとって『sub-agent の支出は無かった』と伝えることになる」として拒否している当の値である。**
同じ関数の中で、同じ出力が、一方では拒否され一方では既定値として使われている。

**UNVERIFIED（latent）**: `serde_json::to_string` がここで失敗する入力を構成できなかった
（`Value::Array` と `json!` マクロ製の `Value` は循環参照を作れず、f64 の NaN/Inf は
`serde_json::json!` の時点で `Null` になる）。blastguard §5 の `sort_output_file` と同じく
**NON-EXPLOITABLE な conflation として記録し、live な fail-open とは呼ばない**。
ただし**是正コストがほぼゼロ**（`Err` で exit 1）なので、修正候補としては残す。

---

### P12. `gauge install` が home 解決失敗時に `./.claude/settings.json` へ書いて成功を報告する【CONFIRMED（コード読解）/ LOW】

役割: **設置経路**（verdict 経路ではない）。

```rust
// crates/gauge/src/install.rs:13-25
fn settings_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".claude")
        .join("settings.json")
}

fn binary_path() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(String::from))
        .unwrap_or_else(|| "gauge".to_string())
}
```

そして:

```rust
// crates/gauge/src/install.rs:69-71
    write_settings(&settings)?;
    println!("\nInstalled Stop hook → {bin} record");
```

home が解決できなければ hook は**どこにも設置されない**のに `Installed Stop hook` と報告する。
`current_exe` が取れなければ PATH 上の裸の `gauge` を書く。

**budgetguard 監査 §7 が同一クラスの 2 件（`crates/budgetguard/src/install.rs:12-13` / `:19-22`）を
「未是正・backlog 済み」として記録しており、gauge にも同じものがある**
（既存 backlog `1e783882` の `crates/harness-core/src/boundary.rs:186` の裸 `overwatch` と同一クラス）。
verdict 経路ではないので本監査の完了条件の外だが、**設置されないゲートは fleet 規模の fail-open** である。

---

### P13. `window::load` が「未登録」と「壊れている」を同じ `None` に写す【CONFIRMED（コード読解）/ LOW】

役割: **産出(P-site)** ＋ `status` の**判定(D-site)**。

```rust
// crates/gauge/src/window.rs:39-42
pub fn load(dir: &Path) -> Option<WindowConfig> {
    let text = std::fs::read_to_string(file_path(dir)).ok()?;
    serde_json::from_str(&text).ok()
}
```

`crates/gauge/src/window.rs:13-17` はこれを宣言済み仕様としている
（"Unset (no file, or unparseable): every caller treats this as `None` … fail-soft"）が、
**帰結は 2 つある**:

```rust
// crates/gauge/src/main.rs:175
            None => println!("no window registered. use `gauge config set-window` to set one."),
```

`gauge config show` は壊れたファイルに対して「**登録されていません**」と**断定**する
（登録されているが読めない、ではなく）。そして:

```rust
// crates/gauge/src/main.rs:640-648
    if let Some(w) = window::load(&config::base_dir()) {
        if let Some(secs) = window::approx_reset_in_secs(&w, chrono::Utc::now()) {
            println!(
                "window eta:   {} (approx, {}h window)",
                …
```

`gauge status` は**行ごと出さない**。CLAUDE.md 第1節が `3b1eb24` を引いて
「statusline の空表示は『余裕あり』と読まれる fail-open だった」と名指しした形と同型の
**沈黙**である。ただし window は README が「unset は normal, expected state, never an error」と
宣言している **D の側面**も持つ。**D と P の境界は「不在」と「破損」の間にあり、
今のコードはその境界を表現できない。** 重大度は低い（消費者は人間のみ）。

---

### P14. `find_transcript(None)` が全プロジェクト横断で最新の transcript を選ぶ【CONFIRMED（コード読解）/ LOW】

役割: **産出(P-site)**。

```rust
// crates/gauge/src/main.rs:411-436（抜粋）
        for f in entries {
            …
            if best.as_ref().map(|(t, _)| modified > *t).unwrap_or(true) {
                best = Some((modified, p));
            }
        }
    }
    best.map(|(_, p)| p)
```

`session_id` が `None` のとき、`~/.claude/projects/*/` **全体**で最も新しい `*.jsonl` を選ぶ。
condukt の 2 つの消費者は `--session` を**渡していない**:

```rust
// crates/condukt/src/state.rs:2248-2250
    let out = std::process::Command::new("gauge")
        .args(["subagents", "--json"])
        .output()
```

```rust
// crates/condukt/src/state.rs:2281-2283
    let out = std::process::Command::new("gauge")
        .args(["subagents", "--json"])
        .output()
```

並行セッション下では**別セッション・別プロジェクトの transcript** が選ばれうる。
実害は多くの場合「agent_id が一致せず `None` → 手動値へ fallback」に留まる（degrade であって
fail-open ではない）が、**agent_id が衝突した場合に他セッションのコストを自タスクに帰属させる**。
`crates/condukt/skills/condukt/SKILL.md:1006` は `${SID:+--session "$SID"}` で正しく渡しているので、
**Rust 側と SKILL 側で作法が食い違っている**。

---

## 3. R — 判定不能を制限側へ解決している経路（**変更しない**）

**見た目が permissive なので、grep ベースの一括修正で誤って「直される」危険がある。**
budgetguard 監査 §3 / autoflow の lock と同じ、**grep の見た目と意味が逆転する**類
（`crates/autoflow/src/lock.rs:32` 「Cannot-determine resolves to」。
**なお budgetguard 監査 §3 がこの先例に付けている行番号は既に drift しており、
現在の `crates/autoflow/src/lock.rs:81` は `holder and one-or-more registered drivers, and the explicitly` という別の docstring 行である** — 行番号つきの引用は転記した瞬間から腐る、という
CLAUDE.md 第3節の警告の実例なので、本監査では再測定した行を使う）。
**この drift はこのリポジトリのゲート自身が検出している**: `scripts/check-doc-claims.py` は
`docs/autoflow-verdict-audit.md:422` の当該引用を `line-drifted (quote is at line(s) 94, 144 but the
claim cites 81)` と報告する（exempt 扱いなので block はしない）。実体の逆転サイトは
`crates/autoflow/src/lock.rs:94` `Some(v) => !v.get("stale").and_then(|s| s.as_bool()).unwrap_or(false),`
であり、`unwrap_or(false)` は `!` を通って「**active＝stand down**」＝制限側に着地する。

| # | 位置 | 逐語 | なぜ restrictive か | 実測 |
|---|---|---|---|---|
| R1 | `crates/gauge/src/main.rs:212-223` | `let aggregate = match est.aggregate.complete() { … Determination::Undetermined(why) => { eprintln!(…); return; } }` | sub-agent 走査が失敗した aggregate を**永続化しない**。budgetguard は record 不在 → transcript 再パースへ倒れる | Probe R1 / 4-3（dir 粒度では block まで届く） |
| R2 | `crates/gauge/src/main.rs:573-584` | `Determination::Undetermined(why) => { eprintln!("gauge: session store unknown — …"); std::process::exit(1); }` | store が読めないとき空の report を出さず **exit 1** | Probe B4 |
| R3 | `crates/gauge/src/main.rs:620-623` | `Determination::Undetermined(why) => (format!("unknown ({why})"), "unknown (session store unreadable)".to_string())` | `status` が `0` / `$0.00` ではなく **`unknown`** と明示。CLAUDE.md 第1節の statusline 教訓の正しい適用 | — |
| R4 | `crates/gauge/src/main.rs:506-517` | `Determination::Undetermined(why) => { eprintln!(…); … std::process::exit(1); }` | `subagents` の dir 走査失敗を **exit 1**。condukt の soft probe が fallback する | Probe R1b |
| R5 | `crates/gauge/src/main.rs:451-459` / `:470-485` | `if secs < 0 { None } else { Some(secs) }` / `"elapsed_secs": span_secs(…)` | 未観測の活動時間を `0` や「今」ではなく **null** にする。`absent_timestamps_render_null_never_zero` が固定 | — |
| R6 | `crates/gauge/src/main.rs:546-552` | `None => "time unknown".to_string(),` | 人間向けにも `0s` ではなく **`time unknown`** | — |
| R7 | `crates/gauge/src/config.rs:116-120` | `std::env::var("GAUGE_DISABLE").map(\|v\| !v.is_empty() && v != "0").unwrap_or(false)` | **`false` は「無効化されていない」＝記録は armed のまま**。未設定・不正値で記録が止まらない。budgetguard R（`BUDGETGUARD_DISABLE`）と同一 | — |
| R8 | find_transcript の最新候補選択（逐語は表の下） | `unwrap_or(true)` は**初回候補の採用**（`best` が `None` のときだけ真） | 判定ではない | — |
| R9 | `crates/gauge/src/main.rs:335` | `report::money(costs.get(name).copied().unwrap_or(0.0)),` | `costs` は直前の `rec.agent_costs()` が**同じ `rec.agents` から**構築するので全キーが存在する。全域関数であり `unwrap_or` は到達しない | — |
| R10 | `crates/gauge/src/report.rs:150` / `:168` / `:190` | `.unwrap_or(std::cmp::Ordering::Equal)` | NaN コストの**並び順**にしか影響しない。合計にも判定にも入らない | — |
| R11 | `crates/gauge/src/main.rs:139` | `Command::Record => run_hook(record_hook),` | **条件つき R**。panic → record 不在 → C1 が transcript 再パースへ fallback（正確な情報源）。§0 参照。**この免責は budgetguard 側の fallback が存在し続けることに依存しており、gauge 側は何も強制していない** | Probe 4-1（record 無しでも block $270.0075） |
| R12 | `crates/harness-core/src/session.rs:135-161` | `pub fn upsert(…)` の無音失敗 | **消費者を列挙したうえで**宣言された免責（CLAUDE.md 第1節が要求する形式を満たす数少ない例）。書き込み欠落は「transcript から再導出」へ degrade する | — |

R8 の逐語（`|` を含むため表の外に置く）— `crates/gauge/src/main.rs:433` `if best.as_ref().map(|(t, _)| modified > *t).unwrap_or(true) {`

---

## 4. D — 意図的に許可へ解決する宣言済み仕様（permissive ではない）

| 位置 | 挙動 | 宣言箇所 / 固定テスト |
|---|---|---|
| `crates/gauge/src/config.rs:116-120` ＋ `crates/gauge/src/main.rs:188-190` | `GAUGE_DISABLE=1` で記録を完全停止 | README「`GAUGE_DISABLE=1` turns recording off entirely」。panic barrier の**外側**で評価され常に到達可能 |
| `crates/gauge/src/config.rs:73-81` | config ファイル**不在** → 内蔵既定値 | `crates/gauge/src/config.rs:1-3` の module header。**ただし「不在」と「読めない」を区別していないので、この D は P3 と分離できていない** |
| `crates/gauge/src/report.rs:78-80` | record 0 件 → `"no sessions recorded yet. …"` | `empty_report` テスト。**正直な空状態**。ただし P5 が偽の空をここへ合流させる |
| `crates/gauge/src/report.rs:47-57` | `cache_hit_rate` の分母 0 → `0.0` | docstring が宣言。`crates/budgetguard/src/cache.rs:11` 「Why this does NOT call gauge's」が、判定に使う側は**この関数を再利用しないと明示的に選んでいる**（理由は続く `:12` 「input at all" with」— 入力ゼロを `0.0` と答えるのは表に出すには妥当でも閾値に食わせるには誤り、と述べている）— **判定に使う側が自分で clamp する**という正しい分離 |
| `crates/gauge/src/window.rs:13-17` ＋ README | window 未登録 → uptime のみ表示 | README「a normal, expected state, never an error」 |
| `crates/gauge/src/main.rs:191-194` | 空 / 解析不能 stdin → 無音 return | 統合テスト `record_hook_exits_zero_on_malformed_stdin`。下流は record 不在 → transcript 再パース（R11 と同じ論拠） |

**D と P の違いは「許可するか」ではなく「許可を*決めた*か」である**（budgetguard 監査 §4 の定式化）。

---

## 5. 棄却した候補（記録する — CLAUDE.md 第6節）

**棄却にも発見と同じ立証責任を課す。** 記録しないと次の読み手が再提起する。

| 候補 | 読みでの予測 | 判定 | 根拠 |
|---|---|---|---|
| `crates/gauge/src/report.rs:26-27` `let c = if c.abs() < 1e-9 { 0.0 } else { c };` が微小コストを $0 に丸める | fail-open | **REFUTED（NON-EXPLOITABLE）** | 閾値は 1e-9 USD＝opus 入力 0.0002 トークン相当。丸めは**表示のみ**で、`total_cost` の加算は `record_cost` の生の f64 で行われる（`crates/gauge/src/report.rs:96-98`）。判定に入らない |
| `subagents` の人間向けヘッダの `file_stem` fallback（逐語は表の下） | fail-open | **REFUTED** | 人間向けヘッダのラベル 1 個。JSON 出力にも判定にも入らない |
| `crates/gauge/src/main.rs:560-562` `.or(s.agent_type.as_deref()).unwrap_or("")` | fail-open | **REFUTED** | 同上（人間向け列） |
| `crates/gauge/src/report.rs:219-226` `truncate` | fail-open | **REFUTED** | 表示幅のみ |
| `crates/gauge/src/main.rs:586-592` `duration_secs` の `.max(0)` が逆転した span を `0` に clamp | fail-open | **RECORDED, not asserted** | `span_secs`（R5）は逆転を `None` にするのに、`duration_secs` は `0` に clamp する — **同じ crate 内で方針が食い違っている**。ただし消費者は `session` の人間向け表示 1 箇所（`crates/gauge/src/main.rs:308-310`）のみで、判定には入らない。`crates/gauge/src/main.rs:446-450` の docstring がこの非対称を**意図として明記**しているので P には数えない |
| `crates/gauge/src/config.rs:57` `pricing: Vec::new()` （`Default`） | 空集合 fail-open | **REFUTED（単独では）** | 「override なし」は**内蔵表へのフォールバック**であり、より permissive にはならない。**ただし P3 / P4 と組み合わさると** override が消えて価格が下がるので、そちらで数えている |
| `record_hook` の `HookInput::parse` → `None` 無音 return（`crates/gauge/src/main.rs:192-194`） | fail-open | **REFUTED（条件つき）** | 下流 C1 は record 不在で transcript 再パースへ fallback（Probe 4-1 で record 無しでも `$270.0075` の block を観測）。**ただしこれは budgetguard 側の性質であり、gauge 側は何も保証していない**（R11 と同じ caveat） |
| `crates/harness-core/src/hook.rs:245-249` `run_hook` の常時 exit 0 | fail-open | **REFUTED** | CLAUDE.md 第1節が明示する carve-out（判定を持たない hook 用の入口）。gauge の record は §0 の分析どおり、欠落しても消費側が restrictive に倒れる。**ただし「部分的な record」は救われない → P1** |

上の 2 行で表から外に出した逐語（`|` を含むため）:

- `crates/gauge/src/main.rs:538` `path.file_stem().and_then(|s| s.to_str()).unwrap_or("")`

---

## 6. 未検証で残したもの（この監査の境界 — 隠さず記す）

| 項目 | 状態 | 理由 |
|---|---|---|
| **P8**（`Path::exists()`） | **UNVERIFIED** | 移植可能な fault injection を構成できなかった（cwd 自身の権限を落とすと probe が cd できない）。budgetguard 監査 §7 が同じ制約を記録している。**REFUTED ではない** |
| **P10**（`current_dir()` 失敗） | **UNVERIFIED** | 削除済み cwd の再現が macOS 上で安定しなかった |
| **P11**（直列化失敗） | **UNVERIFIED（latent）** | 到達する入力を構成できなかった。到達不能を**証明**したわけではないので REFUTED にしない |
| census 37 サイトのうち、本文書が個別に分類していないもの | **未分類** | §2/§3/§5 で逐語引用したのは 37 のうち約 30。残余（主に `report.rs` / `main.rs` の表示系 `unwrap_or`）は**カテゴリで却下していない**が個別検証もしていない。**blastguard §3.3 の教訓（カテゴリ却下は個別検証ではない）に従い、「clean」とは呼ばない** |
| gauge の **write** 経路の並行性 | **未監査** | `session::upsert` は lock を取らない。2 セッションが同時に Stop したとき同一 session_id の record は 1 つなので lost update は起きないが、**別 session_id なら別ファイル**なので競合しない、という読みは検証していない |
| `harness-status` / `session-insights` 側の消費経路の完全監査 | **範囲外** | §0 で消費者として列挙し、P1 が `crates/session-insights/src/record.rs:97-100` の docstring を偽にすることまでは実測で示した。両 crate 自身の verdict 経路は別監査 |
| **利害関係の非独立性** | **明示する** | CLAUDE.md 第2節 (a) は「テストは利害のない Agent が書く」を要求する。本監査は**単一の Agent が発見と検証の両方を行った**。代償として**すべての注入に対照（アンチ空虚）を置いた**（Probe 4 の case 1/3、Probe 2 の CONTROL/CONTROL2、Probe B の B0/B4、Probe C の C0、Probe A の A0）が、**同一主体の盲点を共有するリスクは残る**。独立モデルによる再検証を推奨する |

---

## 7. 修正の優先順位（本監査では実施しない）

本監査は read-only であり、以下は**着手していない**。優先度は
「判定を持つ下流に届くか × 起こりやすさ × 修正コスト」で並べた。

1. **P1**（CRITICAL）— sub-agent transcript の**ファイル**単位 read 失敗。
   **【2026-09-24 実施済み — backlog `fbb3100a`】** 以下の推奨どおりに修正した。
   `aggregate()` の個別 read は `Err` で `agg.subagent_scan` を `Undetermined` に倒し
   （残りのファイルは畳み続ける）、`subagent_usage()` は `map` のクロージャから
   早期 return できないので `match` へ展開したうえで `Undetermined` を返す。
   `crates/harness-core/tests/subagent_file_read_undetermined.rs` が両方向を固定する
   （注入 2 本 + 対照 2 本。RED を先に観測: 修正前 2 passed / 2 failed → 修正後 4 passed）。
   `crates/session-insights/src/record.rs` の docstring も同じコミットで実挙動に合わせた。
   **以下の引用は修正前のコードであり、現在の実装とは一致しない**（監査時点の記録として残す）。

   `{"decision":"block"}` を実測で消す唯一の項目。`crates/harness-core/src/usage.rs:191` の
   `if let Ok` に `Err => agg.subagent_scan = Determination::undetermined(…)` を足すだけで、
   既存の guard（R1）と budgetguard 側の受け口（Probe 4-3 で動作確認済み）がそのまま効く。
<!-- doc-claim-exempt: 修正前のコードの逐語引用。fbb3100a で当の行を書き換えたので index とは一致しない。監査時点の記録として意図的に残している -->
   併せて `crates/harness-core/src/usage.rs:412` `let Ok(text) = std::fs::read_to_string(&file) else {` も `Undetermined` へ倒し、
   `crates/session-insights/src/record.rs:97-100` の docstring を実挙動へ合わせる。
2. **P2**（HIGH）— 未知 model の $0 価格付け。`rate_for` を `Determination<Rate>` 化。
   影響範囲が広い（gauge 8 サイト＋budgetguard＋harness-status＋session-insights）ので
   段階移行になるが、**model id は外部が変える入力**であり、放置は「いつか静かに無効化される gate」。
   `unknown_model_is_free` は**逆向きの assert へ書き換える**必要がある（仕様として固定されている）。
3. **P4**（HIGH）— `crates/gauge/src/config.rs:104-105` の `unwrap_or(0.0)`。**最も安い修正**:
   `pattern` と同じく `r.input?` / `r.output?` にして不完全な行を落とすか、
   `Determination` で全体を undetermined にする。budgetguard は既に後者と同値。
4. **P3**（HIGH）— `Config::load` を `Determination<Config>` 化（budgetguard P1/P2 の移植）。
   「不在＝Known(default)」と「在るが読めない＝Undetermined」の線引きは budgetguard が
   `an_absent_config_is_known_default_not_undetermined` で確立済みなので、そのまま踏襲できる。
5. **P5**（MEDIUM-HIGH）— `crates/harness-core/src/session.rs:220-224` の file 粒度 skip。
   `Determination<Vec<…>>` は既にあるので、skip した件数を `Undetermined` に倒すか、
   `Known` に「不完全である」という印を足す。P1 と同じ設計判断なので**同時に直すのが自然**。
6. **P6 / P7**（MEDIUM）— `session --json --session <ID>` と `subagents`（transcript 不達）を
   `crates/gauge/src/main.rs:19-21` の宣言どおり `unknown` + 非 0 に揃える。`known_records`（R2）と
   `Determination::Undetermined` 枝（R4）が既にあるので、**経路を 1 本ずつそこへ寄せるだけ**。
   `find_transcript` を `Determination<Option<PathBuf>>` にするのが筋。
7. **P9 / P11**（LOW-MEDIUM）— `--since` の `unwrap_or(false)` と直列化失敗の `{}`/`[]`。
   いずれも 1〜2 行。**P11 は同じ関数の中に正解が書いてある**ので、放置する理由がない。
8. **P8 / P10**（UNVERIFIED）— `try_exists` 化と `current_dir` 失敗の exit。
   budgetguard に是正済みの前例があるので移植できるが、**先に観測を作れるかを確かめる**こと
   （テストが書けないなら CLAUDE.md 第2節に従い ask する）。
9. **P12 / P13 / P14**（LOW）— 設置経路と window と cross-session transcript 選択。
   P12 は budgetguard 監査 §7 と同じく**別項目として起票**するのが妥当。

---

## 8. 一般化する所見

この crate 固有ではない 3 つ。

> **(1) 三値化は「一段上の粒度」で止まりがちである。**
> gauge / harness-core の 3 箇所（`aggregate` の sub-agent 走査、`subagent_usage`、`load_all`）は
> **ディレクトリの失敗を三値で表現し、そのディレクトリの中身の失敗を二値で捨てている**。
> P1 / P5 / P7 はすべてこの形である。しかも**正しい側と間違った側が同じ `match` の中で隣接している**。
> 監査の問いは「三値か」ではなく「**どの粒度まで三値か**」であるべきだった。

> **(2) 免責の docstring は、書いた crate の外で偽になる。**
> `crates/gauge/src/main.rs:16-18` は「record は不完全な aggregate を永続化しない」と宣言し、
> `crates/session-insights/src/record.rs:97-100` はその宣言を**根拠として引用**して自分の
> 警告表示を省いている。**宣言が一段の粒度で偽になった瞬間、引用した側も一緒に偽になる。**
> 「別 crate の docstring を根拠に自分の検査を省く」パターンは、それ自体が監査対象である。

> **(3) 「未知のものは 0 として扱う」は、コスト領域では常に fail-open である。**
> P2（未知 model → $0）・P4（欠落 rate → $0）・P5（読めない record → 合計から消える）・
> P9（日付不明 → 期間外）・P1（読めない子 → 支出ゼロ）は、**すべて同じ 1 つの誤りの 5 つの現れ**である。
> 予算という領域では、**測れなかったものを 0 に写すことは、常に上限を無効化する方向**にしか働かない。
> `crates/blastguard/src/model.rs:5`「Three answers, not two.」が要求している第三の答えは、
> ここでは `unknown` という**金額ではない値**である。

---

*本監査は `crates/gauge/` 配下を 1 行も変更していない。P1〜P14 はすべて未是正である。*
