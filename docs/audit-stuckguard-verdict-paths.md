# stuckguard — per-gate verdict-path 監査

DoD9 の 6 本目（schemaguard / autoflow / budgetguard / mutategate / propguard に続く）。
**未分類 0 件**。カテゴリ単位の棄却は行わない（CLAUDE.md 第6節）。

## 0. 要旨

| | |
|---|---|
| 分母（census） | **35** サイト |
| 制限側へ解決済み | **34** |
| 実在する fail-open | **1**（本コミットで修正） |
| 監査対象外と判断 | 0 |

**stuckguard は block しない gate**（`docs/gate-taxonomy.md` の protect-the-protector）。
判定は nudge の emit であり、その「制限側」＝ **nudge を出す**方向。
したがって本監査で問うのは exit code ではなく **「沈黙が『進捗している』と読まれる経路はどこか」**である。
CLAUDE.md 第1節が言うとおり、判定を持つかは返り値の型ではなく**消費のされ方**で決まる。
stuckguard の消費者は LLM 自身で、nudge が出なければループはそのまま続く — 沈黙は
「問題なし」と等価に読まれるので、この crate は判定を持つ側である。

## 1. 分母

```
python3 scripts/census-verdict-terminals.py stuckguard
```
→ 35 サイト（測定日 2026-08-04、測定点は本コミットの作業ツリー）。
内訳: config.rs 2 / detect.rs 1 / install.rs 3 / main.rs 4 / sig.rs 22 / state.rs 1。
**行番号は drift する** ので上記コマンドで再測定すること。

## 2. 制限側へ解決している 34 サイト

### A. 「判定不能 → nudge を出す」に倒れている — 2 件

| 場所 | 潰し方 | なぜ制限側か |
|---|---|---|
| state.rs `in_cooldown` | `.unwrap_or(false)` | 履歴が引けない → cooldown 中ではない → **nudge は抑制されない** |
| config.rs `disabled_env` | `.unwrap_or(false)` | env 読取不能 → **無効化しない** ＝ gate は動き続ける |

### B. 設定読み込みの degrade（宣言された advisory carve-out）— 1 件

config.rs `load` の `boundary::read_to_string(&path).require().ok().flatten()` は
`Known(None)`（読む直前に消えた）と `Undetermined`（あるが読めない）の**両方**を
組み込み既定へ落とす。コード自身がその理由を明記している:

> stuckguard is a pure advisory hook (never blocks), so config-load failure degrades to defaults rather than escalating.

**方向を実際に確かめた**（宣言を鵜呑みにしない）: 既定へ落ちると `enabled = true`、
`repeat_threshold` は既定値（小さい方）に戻る。つまり設定を読めない時の縮退は
**より多く nudge する**側であり、制限側と一致する。設定で gate を緩めていた利用者は
その緩和を失うが、それは安全側の失い方である。

### C. signature 構築の欠損フィールド — 22 件（sig.rs）

`field(&inp, "command").unwrap_or("")` 系。ツール入力に期待したフィールドが無いとき
空文字にする。**方向**: 欠損フィールド同士は同じ空文字へ正規化されるので `sig` が
**衝突しやすくなり**、`repeat` 検出は**より発火しやすく**なる。制限側。

`sig.rs` の `let error = response.map(looks_error).unwrap_or(false)` と
`looks_error` の `_ => false` は逆向き（response 不明 → エラーでないと見なす）だが、
これは `all_errored` と `failed_test_digest` にしか効かず、**repeat/oscillation の
検出そのものは error 状態と独立**に動く。落ちるのはエスカレーションの色付けだけで、
nudge の有無ではない。

### D. 判定を持たない経路（下流消費者を列挙）— 9 件

| 場所 | 下流消費者 | 免責の根拠 |
|---|---|---|
| install.rs `settings_path` / `binary_path`（3件） | `stuckguard install` の settings.json 書き込み | インストーラ。hook 判定経路に無く、失敗は install の出力で可視 |
| main.rs `status` の `current_dir` | 人間向け表示 | 表示専用 |
| main.rs `lesson_query` の `None => t.detail.clone()` | 過去 lesson の検索クエリ | 検索が外れても nudge は出る。lesson は nudge の**装飾** |
| main.rs `record_lesson` の `.unwrap_or(0)` | lesson のタイムスタンプ | 時刻が 0 でも lesson は記録される |
| main.rs `watch` の `repeat_streak.unwrap_or(nudge_count)` | 表示件数とエスカレーション判定 | streak が無い（Oscillation）ときは nudge 件数を使う宣言された分岐。どちらも単調増加なのでエスカレーションは遅れこそすれ消えない |
| detect.rs `jaccard` の `union == 0 => 0.0` | 近似 repeat 判定 | CA-stuckguard-02 で**意図的に**この向きへ倒した（空トークン同士を完全一致とみなす方が誤検出を生んだ）。宣言済みの仕様 |

## 3. 実在した fail-open（本コミットで修正）

### 使えない三つ目の signal を測定値として平均に入れ、advisory を到達不能にしていた

`progress_score` の docstring は一貫してこう約束していた:

> `progress_score` is the unweighted mean of the three signals actually available

実装は **無条件に 3 で割っていた**:

```rust
let score = (diversity_signal + stability_signal + error_recurrence_signal) / 3.0;
```

`error_recurrence_signal` は、現イベントに `failed_test_digest` が無いとき
`None => 0.0` になる。これは**測定された 0 ではなく「測れなかった」**である。
それを測定値として平均に入れると、score の上界は

```
(diversity + stability + 0) / 3  ≤  ((1 - 1/N) + 1) / 3  =  (2 - 1/N) / 3   →   sup = 2/3 ≈ 0.667
```

一方 advisory は `score >= progress_score_threshold`（既定 **0.75**）で発火する。
**0.667 < 0.75 なので、error digest を持たないウィンドウでは advisory は数学的に発火し得ない。**
＝ Read / Grep / Edit を繰り返すような**非エラーのループ全部**で、この advisory は
死んだコードだった。しかもそれは、この advisory が捕まえるために存在するループ形そのものである。

実測（修正前、window=12 の全同一 Bash イベント＝ digest 無しで最も停滞した形）:

```
scored 0.6388888888888888 but the advisory fires at 0.75
```

**なぜこれが第4節の事案か**: 散文は正しい仕様（"actually available"）を書いており、
実装だけがそうなっていなかった。読んだ人は仕様どおりだと信じる。
`docs/GLOSSARY.md` の fail-open 定義そのままに、「検査できなかった」が
「検査して問題なし」と同じ出力（沈黙）へ写っていた。

**修正**: `error_recurrence` を `Option<f64>` にし、利用可能な signal だけで平均する。
`Some(digest)` かつ該当エラーが無い場合の `0.0` は**測定された 0**（前進の証拠）なので
平均に残す — 「未評価」と「評価して 0」を型で分けたのが修正の本体である。
`error_recurrence_signal` フィールドは後方互換のため 0.0 を報告し続けるので、
フィールドは「recurrence の寄与」であって「recurrence を測った」ではない旨を docstring に明記した。

**挙動変化（隠さず記す）**: 修正後は digest 無しでも advisory が発火しうる。
分母が 2 になるので、例えば diversity 0.5・stability 1.0 のウィンドウは 0.75 に達する。
これは意図した変化（発火可能にすることが修正の目的）だが、**advisory の頻度は上がる**。
対照実験として、12 個の相異なるアクションのウィンドウは 0.05 に留まることを固定した。

## 4. F→P 証跡

修正前に RED を観測（上の実測値）。同時に対照 2 本は緑のままだった:

```
an_all_identical_non_error_window_is_scored_as_stalling   FAILED   (0.639 < 0.75)
a_diverse_window_is_not_scored_as_stalling                ok       ← 反空虚対照
a_recurring_error_digest_still_contributes_its_signal     ok       ← 三signal 経路の非回帰
```

反空虚対照は必須である: `progress_score` が常に 1.0 を返す実装、あるいは第三 signal を
**常に**捨てる実装は、最初のテストを満たしながら健全な作業まで停滞と宣言してしまう。
12 個の相異なるアクションが 0.05 に留まることを固定して、それを塞いだ。

修正後: 64 + 4 + 11 テスト green、clippy 0 warnings。

## 5. この監査が証明しないこと

- `harness_core::boundary` の内部は監査していない（harness-core 自身の単位）。
- `progress_score_threshold` の既定値 0.75 が**正しい**閾値であることは示していない。
  示したのは「修正前は digest 無しで到達不能だった」ことと「修正後は到達可能」であること。
  適正値は運用データで測るべきで、それは本コミットのスコープ外。
- CLAUDE.md 第2節(a) の逸脱: テストは修正と同じ agent が書いた（本セッションの system prompt が
  Agent 起動を禁じるため）。RED の先行観測と反空虚対照で代償したが、独立再監査を推奨する。
