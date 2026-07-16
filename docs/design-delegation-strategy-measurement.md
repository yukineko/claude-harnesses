# DESIGN: delegation戦略(fork vs inline)の計測基盤をfugu-routerに追加する

**前提**: `docs/fork-subagent-type.md`を先に読むこと(forkの定義・context rotとの関係・
監査独立性の観点でforkを使ってはいけない場面を先に整理してある)。

**背景**: `/flow`が`/condukt`を起動する際、実行そのものを`fork`(subagent_typeの一種。親会話の
contextを丸ごと継承し、prompt cacheも共有する)に包むか、mainで直接実行するかという選択がある。
今回のセッションでの分析(会話ログ参照)では、forkはcontext-rot対策として有効な一方、
「本当にコスト最適か」は定性的判断に留まっていた。fugu-routerは既にモデル階層選択を実績データ
(`Episode`ストア)から決定論的に行っている(`route`/`decide_bandit`)ので、delegation戦略の選択も
同じ枠組みで**将来的に**学習させたい。ただし今は比較データが存在しないため、**このタスクの
スコープはあくまで計測基盤の追加**であり、自動判定ロジックの実装は含まない(build ≠ validate、
測ってから方策に反映する既存の設計思想を踏襲)。

## なぜ`condukt shadow-run`(batch4, backlog `cb2aabff`)の仕組みをそのまま使えないか

`crates/condukt/src/shadow_run.rs`の既存shadow-runは「**同一condukt run内**で、同じタスクを
別モデルのworkerで並行実行し比較する」機構。両方の分岐は**conduktの内部**(Taskで起動される
worker)で完結しており、`fugu-router record --class shadow-run`で記録される。

一方「forkで包むか/inlineで実行するか」は、**condukt自身ではなく、conduktを呼び出す側(`/flow`
のskillロジックを実行している側=LLM)の選択**である。conduktの内部からは「自分がforkの中で
動いているか、mainから直接呼ばれたか」を区別する手段が無く、shadow-runのように「1回のcondukt
run内で両分岐を並行実行し比較する」形には**そもそも当てはまらない**。したがって
`shadow_run.rs`の内部ロジック(モデル比較の分岐機構)には一切手を入れない。

## 提案する設計: fugu-routerに軽量な記録手段を追加するだけ

比較すべきは「等価なタスクを、fork経由で実行した場合とinlineで実行した場合の
cost_usd/duration_secs/成否」。これは**conduktの外側**(`/flow`を運用するLLM)が手動で選択し、
実行後に**手動で**`fugu-router record`に記録すればよい。新しい実行機構は不要で、既存の
`record`コマンドに1つオプション引数を足すだけで足りる。

### 1. `fugu-router record`に`--delegation <fork|inline>`を追加(省略可)

`crates/fugu-router/src/main.rs`の`Record`サブコマンドに、既存の`--class`/`--model`と同様の
位置づけで:

```rust
/// このEpisodeがどちらのdelegation戦略で実行されたか(fork|inline)。省略時は記録しない
/// (delegation比較に無関係な通常のworker/verifier実行と揃える。後方互換)。
#[arg(long)]
delegation: Option<String>,
```

`fork`/`inline`以外の値もそのまま文字列として受け付けてよい(class同様、バリデーションは
しない・呼び出し側の自由記述を尊重する既存の緩さに合わせる)。

### 2. `Episode`構造体に`delegation: Option<String>`を追加

`crates/fugu-router/src/store.rs`の`Episode`に、既存の`human_label: Option<bool>`と**同じ
back-compatパターン**で追加する:

```rust
/// このEpisodeがfork経由/inline実行のどちらで生成されたか。手動記録のみ(自動判定なし)。
/// `None` = delegation比較の対象外(通常のcondukt worker/verifier実行)。
#[serde(default, skip_serializing_if = "Option::is_none")]
pub delegation: Option<String>,
```

`#[serde(default)]`により、既存JSONL行(このフィールドが無い過去の記録)はNoneとして
問題なく読み込める(batch3で`duration_secs`に`#[serde(default)]`を足した際と同じ後方互換
パターン)。`skip_serializing_if`により、記録しない限り出力JSONLにフィールドが増えない。

### 3. `/flow`のSKILL.mdに2つのガイドラインを追記する(Step 3-2)

a. **delegation戦略の既定バイアス**(会話で合意済みの方針):
   > `/flow`が`/condukt`を起動する際は既定で`fork`に包む。例外は「タスクがxs級・単発ファイルで、
   > ユーザーが経過を対話的に見たいと明示した場合」のみ直接実行。

b. **手動記録の呼びかけ**(計測ループを回すための最小限の運用ルール):
   > condukt実行が完了したら(fork/inlineいずれでも)、`gauge`等から観測できるcost_usd・
   > duration_secsを、以下のように`fugu-router record`で記録する(既存の`record`呼び出しに
   > `--delegation`を足すだけ。新しいコマンド呼び出しを増やさない):
   > ```bash
   > fugu-router record --title "<task title>" --class "flow-delegation" \
   >   --model <suggested_model> --status <verified|failed> \
   >   --cost <observed_cost_usd> --duration <observed_duration_secs> \
   >   --delegation <fork|inline>
   > ```
   > これは**手動**(shadow-runのような自動比較機構ではない)。狙いは「等価なタスクの
   > fork実行/inline実行の実績を、時間をかけて`fugu-router`のEpisodeストアに貯める」こと。

### 4. hypothesis PDOを1件開く

```bash
hypothesis add "fork経由でのcondukt実行とinline実行とで、同等タスクのコスト(cost_usd)・所要時間\
(duration_secs)に有意な差がある" --goal "delegation戦略選択をfugu-routerのroute/decide_banditに\
将来組み込めるだけの実績データを、flow-delegationクラスのEpisodeとして十分な件数(目安10件以上、\
fork/inline両方が最低3件以上)蓄積し、計測結果に基づいてvalidate/rejectする"
```

- **status**: `open`のまま。今回のタスクでは`validate`/`reject`しない(データが無いため)。
- 十分なEpisodeが貯まった時点で、次回`/flow`のmeasure step (3-1の2)が拾い、実際の記録を
  集計して`validate`/`reject`する(既存の「build ≠ validate」ループにそのまま乗る)。

## 明示的にスコープ外にすること

- `fugu-router route`/`decide_bandit`へのdelegation軸の追加(**実装しない**)。データが無い
  状態でロジックだけ足しても検証しようがない。hypothesis validate後の別タスクとする。
- `condukt shadow_run.rs`の内部比較ロジックの変更(**しない**)。既存のモデル比較機構とは
  無関係。
- 自動的な「context使用率からfork/inlineを自動判定する」仕組み(**実装しない**)。会話内で
  確認した通り、既存の`gauge`/`ctxrot`のcontext可観測性はあるが、これを使った自動判定は
  データが無い今の時点では時期尚早。`/flow`のガイドラインは固定の既定バイアス(常にfork、
  例外のみinline)に留める。

## 受け入れ基準 (done_criteria)

- [ ] `fugu-router record`に`--delegation <VALUE>`(Option、省略可、バリデーション無し)が
      追加されている。
- [ ] `Episode`に`delegation: Option<String>`が`#[serde(default, skip_serializing_if =
      "Option::is_none")]`で追加され、フィールド無しの既存JSONL行が引き続きデシリアライズ
      できることを検証する後方互換テストが追加されている。
- [ ] `--delegation`を指定して`record`した場合にEpisodeへ正しく反映され、`skip_serializing_if`
      により未指定時はJSONLにキーが出力されないことを検証するテストが追加されている。
- [ ] `crates/flow/skills/flow/SKILL.md`のStep 3-2に、上記a(既定forkバイアス)・b(手動記録
      呼びかけ)の2点が追記されている。
- [ ] 上記のhypothesisが1件`open`状態で登録されている(`hypothesis list --status open`で
      確認できること)。
- [ ] `cargo test -p fugu-router`の既存テストが全件pass。
- [ ] 触った2crate(fugu-router: コード変更あり, flow: SKILL.mdのみ)のversionをそれぞれ
      micro以上bumpし、3ファイルlockstepで`check-plugin-versions.py`/`check-version-bumped.py`
      が通ること。
- [ ] `shadow_run.rs`・`route.rs`(または`policy`関連のルーティングロジック)には一切手を
      入れないこと(既存テストが無変更でPASSすることで確認する)。

## レビュー所見(別セッションでの内容レビューより)

設計自体(shadow-runとの切り分け、`route`/`decide_bandit`をスコープ外にする判断、
`human_label`/`duration_secs`と同じback-compatパターンの流用)は妥当。一方で、計測ループが
実際に機能するかという観点で以下のリスクが残っている。次にこのdesignを触るセッションは
着手前に一読すること。

1. **既定forkバイアスによるinlineサンプル枯渇のおそれ。** SKILL.mdの既定は「常にfork、例外は
   xs級・単発ファイルでユーザーが対話的に見たい場合のみ」。hypothesisのvalidate基準は
   「fork/inline**両方**最低3件」だが、例外が稀にしか起きない設計なら、inline側のEpisodeが
   3件貯まるまでに非常に長い時間がかかりうる。計測ループが「例外が起きないから永遠にvalidate
   できない」という詰みに陥らないか、hypothesis側の目安件数を見直す必要が出るかもしれない。
2. **記録ステップが完全に手動でゲートが無い。** `fugu-router record --delegation`を呼ぶかどうかは
   `/flow`を実行するLLM側の「思い出し」に依存しており、これを強制する決定論的な仕組み(Stop
   フック等)が無い。condukt/flow全体の設計思想(「判断はLLM、決定論はバイナリ」)からするとこの
   計測ステップだけがLLM任せになっている。記録漏れが起きても検知できないため、実績が本当に
   10件以上貯まるかは運用の遵守率次第。将来的に`overwatch`か`gauge`側でrecord漏れを検出する
   fail-soft advisoryを足す余地がある。
3. **`--delegation`が無検証のfree-text。** `--class`と揃える意図的な緩さだが、集計時に表記ゆれ
   (`"fork"`/`"Fork"`/全角など)が閾値判定に混入しうる。許容トレードオフとして明記されている
   ため実装ミスではないが、集計コード側で正規化が必要になることは覚えておく。

(参考: レビュー時点のローカル`~/.hypothesis/hypotheses.toml`にはこのdelegation関連の
hypothesisが未登録だった。実装が別セッション側で進行中であれば環境差の可能性が高いが、
着手前に`hypothesis list --status open`で実在を確認すること。)
