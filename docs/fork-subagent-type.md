# fork（subagent_type）— contextの継承・context rot・監査独立性

`docs/design-delegation-strategy-measurement.md` はこの前提知識を踏まえて読むこと。

## forkとは何か

Claude CodeのWorkflowツールが提供する `agent()` 関数には `opts.agentType` という引数があり、
そこに指定できる特殊な値の一つが `fork` である。通常のサブエージェント起動（`general-purpose`
など）は**親の会話を一切見ない**——渡されるのは呼び出し側が手動でテキスト化したプロンプトだけ
だが、`fork` は逆に**親の会話のcontextを丸ごと継承**する。ツール定義上の唯一の明文化された
挙動は次の一点である。

> opts.model overrides the model for this agent call ... Ignored for subagent_type: 'fork' —
> forks always inherit the parent model.

つまり `fork` は「親と同じcontext・同じモデルで分岐する」経路であり、モデルの上書きすら
許さないほど親との結合が強い。

## forkの効能: contextを引き継ぐコストがゼロになる

fork以外の起動（fresh spawn）では、直前の議論の前提・途中まで積んだ推論・握った合意事項を、
呼び出し側が毎回手動で切り出してテキストに落とし込む必要がある。forkはこの手間を丸ごと省略
できる。したがって fork が向くのは「**同一の推論スレッドをそのまま伸ばしたいだけ**」の、浅く
短い継続作業である。

## forkの代償: context rotがそのまま伝播する

長く続いた会話には、脱線・却下した仮説・ノイズになった過去のツール出力が積もる
（context rot）。forkは「contextを一切フィルタせずに継承する」経路なので、rotもそのまま
子エージェントに引き継がれる。fork of fork のように連鎖させると、rotは複利的に蓄積する。

これに対する解毒剤が**蒸留（distill）**——生の履歴を「決定事項・現在の状態・制約・未解決の
問い」だけの圧縮版に落とし込み、そこから継続する発想である。

| 経路 | contextの扱い | rotへの態度 |
|---|---|---|
| `fork` | 親のcontextを丸ごと継承 | フィルタなし、rotも継承 |
| distillしてから継続 | 圧縮済みチェックポイントから再開 | rotに対する防波堤 |
| fresh spawn（自己完結プロンプト） | 継承ゼロ、呼び出し側が手動で選別 | 強制的な手動distill |

## 実例: conduktはforkを避け、蒸留と外部状態で代替している

`docs/condukt-context-flow.md` にある通り、conduktのinterpreter/worker/verifierは
**「この会話を一切見ていない」**——毎回まっさらな新規起動で、main loopが `interface_context`
（grepで事前収集）や `knowledge_context`、`done_criteria` を手動でテキスト化して渡す。段階が
変わるたびに強制distillのゲートを通す設計であり、`fork` は一度も使われない。理由は明記
されている。

> main loopのコンテキストが長くなると応答が遅くなり、コストも上がる。worker・verifierを
> 独立起動することで「重い作業の出力（ビルドログ・diff全文）がmain loopのコンテキストに
> 積み上がらない」という効果がある。

さらにconduktは**contextを介さない第3の経路**も併用する。worker→verifierの受け渡しは
worktree（git）と `condukt state`（JSONL）という**永続状態**を共有メモリとして使う。
verifierはworkerの思考過程を継承する必要がなく、実ファイルと `done_criteria` さえ読めれば
判定できる——「rotを薄めて渡す」のではなく「そもそもcontextに依存しない」解決策である。

`ctxrot` クレートの「context劣化ガード（検出・救済・復元・**蒸留**＋load/pin/drop制御）」は、
上表の「distillしてから継続」を**同一セッションの会話**に対して実装したものにあたる。
conduktの蒸留がエージェント切り替えの瞬間（forkの分岐点になり得る場所）に効くのに対し、
ctxrotの蒸留はmain loop自身の会話に効く、という住み分けになる。

## forkを使ってはいけない場面: 監査・検証

ここまでは「contextをどれだけ引き継ぐか」という**量**の話だった。しかし監査・検証には量では
解決できない軸がある——「**誰の**推論経路を引き継ぐか」という質の問題である。

conduktの `condukt state verifier-model` はこの問題を正面から扱う。

> verifierモデルがworkerモデルと決して一致しないよう解決する（**共有ブラインドスポット対策**）

モデルの一致すら禁止するほど厳格に独立性を守っているので、それより遥かに強い共有——context
そのものの継承——であるforkを、workerからverifierへ使うことは設計上ありえない。理由は
次の2点に集約される。

1. **共有ブラインドスポット**: workerが「これで正しい」と自分を納得させた推論の癖・見落としを、
   そのままverifierも引き継いでしまう。distillで圧縮しても、中身がworker自身の正当化ロジック
   なら意味がない。
2. **自己確認バイアス**: workerの会話には「なぜこの実装で良いか」という説得的な文脈がすでに
   積まれている。そこからforkすると、verifierは中立な第三者ではなく「既に説得された延長」に
   なってしまう。

conduktの `adversarial plan/adjudicate`（N人の独立スケプティックによる反証パネル）や
`consensus plan/vote`（N個の独立サンプルによる多数決）も同じ原則の延長線上にある。forkで
contextを共有した瞬間、N個のサンプルが同じ誤りに相関して倒れ、独立サンプルによる多数決という
手法そのものが無意味になる。

## 使い分けの指針

- **forkが向く場面**: 同一の推論スレッドをそのまま伸ばしたいだけの、浅く短い継続作業
- **distillを挟むべき場面**: contextが育ってきた／矛盾や行き止まりが混ざってきた継続作業。
  生の履歴から直接forkせず、圧縮済みのチェックポイントを経由する
- **fresh spawn（fork厳禁）が必須な場面**: 独立したサブタスクの並列実行、そして何より
  **検証・監査・反証**の役割全般（verifier / reviewgate / precommit-audit / propguard /
  specguard / injectguard / adversarial skeptic など）

監査の価値は「対象と違う視点から見ること」そのものにある。forkは「違う視点」を構造的に
潰してしまうため、distillでは代替できない別の制約として扱う必要がある。

## 次に読むもの

上記は「forkを使うべきか」を質的な理由（context rot・監査独立性）だけで判断した整理である。
`docs/design-delegation-strategy-measurement.md` は一歩進んで、「forkを既定にする」という
判断自体が本当にコスト最適か（fork自体のオーバーヘッド、prompt cache共有の効果、実行時間）を
実績データで裏付けようとする計測基盤の設計であり、本ドキュメントの続きとして読むこと。

## 関連

- `docs/condukt-context-flow.md` — conduktのコンテキスト読み込みフロー
- `docs/context-optimization.md` / `docs/context-optimization-flow.md` — context最適化の3軸
  （Size/Cost/Correctness）
- `docs/design-delegation-strategy-measurement.md` — fork/inline選択のコスト最適性を計測する
  設計（本ドキュメントの後続）
