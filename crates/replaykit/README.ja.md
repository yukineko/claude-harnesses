# replaykit

記録された**実行トレース**を golden replay へ昇格させる回帰テストハーネス。[curate](../curate)（playbook→golden）の兄弟で、curate が *playbook* を golden に変えるのに対し、replaykit は *実行トレース* を golden に変える。

## 目的

replaykit は、[tracekit](../tracekit) が記録した condukt の実行トレースを、[evalkit](../evalkit) が消費できる golden replay ケースへ蒸留する CLI である。

condukt の実行は tracekit によって追記専用の span（`~/.tracekit/<run_id>/spans.jsonl`）として記録される。replaykit はその 1 実行を、順序付きステップと `expect` ブロック（実行の phase 集合・エラー件数・コストを固定したもの）からなる移植可能な**トラジェクトリ要約**へ蒸留し、それを evalkit の golden ケースとして昇格させる。

golden を再生すると固定済みの不変条件が再チェックされるため、回帰（新しいエラー・コストの暴騰・phase の欠落）は次に `evalkit run` を実行したときに golden の失敗として表面化する（CI 配線は無く、自動では走らない）。

**サブスクリプションネイティブ**な設計で、バンドルされた単一の Rust バイナリ（std + serde + clap）だけで動く。API キーもネットワークも不要だ。

## どうして必要か

tracekit には condukt の実行が忠実に記録されるが、それは追記専用のトレースにすぎず、**「検証を通った 1 回の実行」を回帰テストとして再利用する経路がなかった**。

実行のたびに span は蓄積されるものの、それ単体では `input→expected` のテストではない。つまり、一度正しく回った実行が、後の変更でこっそり壊れていないか（新しいエラーが混入していないか、コストが膨らんでいないか、想定した phase を踏んでいるか）を機械的に守る仕組みが欠けていた。

replaykit はこのギャップを埋める。1 実行のトレースを、その phase 集合・エラー件数・総コストを固定した**自己検証可能なスナップショット**へ落とし込み、evalkit の golden として固定する。これがなければ、検証済みの実行はトレースログに埋もれたままで、回帰が検出されないまま忍び込むことになる。

## どう使うか

人間が直接叩く、あるいは CI ゲートとして使う**素の CLI** である。ライフサイクル hook ではない。

中心となるループは record→promote→evalkit だ。

```sh
# 1. condukt の実行が tracekit に記録される → ~/.tracekit/<run_id>/spans.jsonl
# 2. それを管理対象の golden replay データセットへ昇格する
replaykit promote --run my-run-2026-06-28 --root . --dataset replayed
#    evals/replay/fixtures/<id>.json   （移植可能な要約）と
#    evals/replay/replayed.jsonl       （id で重複排除された golden）を書き出す
# 3. evalkit がその golden を実行する（cmd は `replaykit verify <fixture>`）
evalkit run        # 固定済みの不変条件を再チェックする（手動で実行）
```

主なサブコマンド:

- `replaykit extract --run <RID> [--spans <path>] [--out <path|->]` — 実行の span を読み込み（`--spans` 指定、なければ `~/.tracekit/<sanitize(RID)>/spans.jsonl`）、トラジェクトリ要約を組み立てて整形 JSON を `--out`（既定 `-` = 標準出力）へ出力する。壊れた span 行はスキップされる。
- `replaykit verify <fixture.json>` — 管理対象の要約 fixture を読み、その集計値（phase 集合・エラー件数・総コスト）をステップから**再計算**して `expect` ブロックと照合する。静的な読み出しではなく、集計ロジックそのものの自己テストになる。違反は標準エラーへ出力される。
- `replaykit promote --run <RID> [--spans <path>] [--root <dir>] [--evals-dir <name>] [--dataset <name>] [--draft]` — 要約を組み立て、`<root>/<evals_dir>/replay/fixtures/<id>.json` に書き出し、golden 行を `<root>/<evals_dir>/replay/<sanitize(dataset)>.jsonl` に追記する（id で重複排除）。golden の `cmd` は `["replaykit","verify",<rel-fixture>]` で、fixture パスは root からの相対になるため、コミットされた golden は移植可能になる。既定値は `--root` が CWD、`--evals-dir` が `evals`、`--dataset` が `replayed`。`--draft` は受け付けるが現状は**何もしない**（curate との対称性のために予約）。

終了コードは evalkit / trajectoryeval と同じ 0/1/2 のゲート方針に従う。

| code | 意味 |
|------|------|
| `0`  | 再生が固定済みの不変条件と一致した（pass） |
| `1`  | 本物の回帰 / 不変条件違反 |
| `2`  | ハーネスエラー（入力の欠落・読み取り不能・不正） |
