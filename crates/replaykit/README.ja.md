`replaykit promote` は tracekit の実行トレース（`spans.jsonl`）を evalkit の golden ケースへ昇格させる。その golden の `cmd` は昇格した fixture に対して `replaykit verify` を再実行し、昇格したままの fixture では 0、fixture のステップの status をエラーに変えると 1 で終了する。

```sh
replaykit promote --run my-run-2026-06-28 --root . --dataset replayed
#    evals/replay/fixtures/<id>.json   （移植可能な要約）と
#    evals/replay/replayed.jsonl       （golden、同じ run を再昇格しても行は増えない）を書き出す
```

`--spans` を指定しない場合、`replaykit extract --run <RID>` は `~/.tracekit/<sanitize(RID)>/spans.jsonl` から span を読む。
`--spans <path>` を指定するとその path から span を読む。
`--out <path|->` の既定は `-` で、整形 JSON のトラジェクトリ要約を標準出力へ出力する。
壊れた span 行はスキップされる。

`replaykit verify <fixture.json>` は管理対象の fixture の phase・エラー件数・総コスト をステップから再計算し、`expect` ブロックと照合する。
総コストは `expect` がコスト上限を固定している場合にだけ照合される。
違反は標準エラーへ出力される。

`replaykit promote --run <RID> [--spans <path>] [--root <dir>] [--evals-dir <name>] [--dataset <name>] [--draft]` は要約を組み立て、`<root>/<evals_dir>/replay/fixtures/<id>.json` に書き出し、golden 行を `<root>/<evals_dir>/replay/<sanitize(dataset)>.jsonl` に追記する（同じ run を再昇格しても行は増えない）。
`--evals-dir` が既定のとき、golden の `cmd` は `["replaykit","verify","evals/replay/fixtures/<id>.json"]` で、fixture パスは root からの相対になる。
既定値は `--root` が CWD、`--evals-dir` が `evals`、`--dataset` が `replayed`。
`--draft` は受け付けるが現状は**何もしない**。

- `0`: 再生が固定済みの不変条件と一致した（pass）
- `1`: 本物の回帰 / 不変条件違反
- `2`: ハーネスエラー（fixture または spans ファイルの欠落・読み取り不能、あるいは要約 JSON として不正な fixture）

素の CLI の**ゲート**であり、ライフサイクル hook ではない。

`[dependencies]` テーブルにあるのは `harness-core`・`serde`・`serde_json`・`clap` だけで、自身の `src/` には `TcpStream`・`UdpSocket`・`reqwest`・`ureq`・`hyper`・`API_KEY` への参照が無い。

このリポジトリに `.github/` の CI は存在せず、`.githooks/` 直下のどのファイルも `evalkit` も `replaykit` も含まない。
