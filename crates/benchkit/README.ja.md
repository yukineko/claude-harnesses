`download` は `cargo test` 中にネットワークへ到達しない。.
benchkit はデータセットを再配布しない: `tests/fixtures/` 外にデータセットファイルは追跡されておらず、`tests/fixtures/instances.jsonl` は 2 行である。.
`download` と `run-instance --real` 以外では benchkit 自身のコードはプロセスを起動せずソケットも開かず、`load`、`--real` 無しの `run-instance`、`audit-sample` (`--audits` / `--from-violations`) は外部ツールを起動しない。.
`audit-sample` の批准キューは自動適用しない。.

benchkit のサブコマンドは `load`、`download`、`run-instance`、`audit-sample` である。.
`dashboard` はサブコマンドではなくライブラリ・モジュールで、1 run を `runs.jsonl` に 1 行追記し、既存行を書き換えない。.
`run-instance` の背後のパッチ生成器は空の候補を返すスタブである。.

ローダーはプロセスを起動せず、時刻も環境変数も読まず、同じファイルを 2 回読むと同じ `Vec<Instance>` になる。.
ローダーはインスタンスをファイル順に返す。.
不正行は `path:line` 付きでエラーになる。.

`instance_id`、`repo`、`base_commit`、`patch`、`test_patch`、`problem_statement` は必須で、どれかを欠く行は読み込みに失敗する。.
`hints_text`、`created_at`、`version`、`environment_setup_commit` は欠けていれば空文字列になり、`created_at` は原文のまま保持される。.
`fail_to_pass` (JSONL キー `FAIL_TO_PASS`) は通るべきテスト、`pass_to_pass` (JSONL キー `PASS_TO_PASS`) は通り続けるべきテストの一覧で、どちらかの ID が失敗すると instance は unresolved になる。.
モデルは serde の `rename` で大文字キー `FAIL_TO_PASS` / `PASS_TO_PASS` を snake_case Rust フィールドへ写す。.
fixture はこれらを素の文字列 JSON リストで持つ。.
`download` は huggingface.co の上流 `.parquet` ファイルをそのままのバイト列でキャッシュパスへ書き、JSONL へは変換しない。.

```sh
benchkit load crates/benchkit/tests/fixtures/instances.jsonl

benchkit download                 # .benchkit-cache/swe-bench-verified.jsonl へ書く
benchkit download --dest data/verified.jsonl   # --dest でキャッシュファイルのパスを指定する
benchkit download --force         # キャッシュがあっても再取得

# --real 無しは dry run: git も pytest も走らず、テスト結果は生成されない。
benchkit run-instance <instances.jsonl> <instance_id>
benchkit run-instance <instances.jsonl> <instance_id> --real   # git clone/checkout/apply + python -m pytest

benchkit audit-sample <changes.jsonl> [--audits <verdicts.jsonl> | --from-violations] \
    [--fraction 0.1] [--seed 0] [--json]
```

`run-instance --real` は `git clone`、`git checkout <base_commit>`、`test_patch` の `git apply`、`FAIL_TO_PASS` と `PASS_TO_PASS` の ID を対象にした `python -m pytest` を順に実行し、pytest 出力の解析結果を採点する。.
結果マップに無いテスト ID は「通っていない」と数える。.

`audit-sample` は seed 固定の決定論的サンプルを引く。.
判定 (`--audits`) または実際の overwatch violation ストリーム (`--from-violations`) を与えると、見逃しを新 invariant 候補と閾値調整の批准キューへ振り分ける。.
`--audits` と `--from-violations` は同時に指定できない。.

`download` はキャッシュ存在時は `--force` が無い限り no-op。.
`[dependencies]` に HTTP クライアント crate を持たず、`curl` にハードタイムアウト (`--max-time 120`) 付きで shell out する。.

<!-- record: 2026-07-08 rev: 999af47e -->
- **データセット:** SWE-bench Verified — SWE-bench の人手検証済み 500 タスク部分集合。
- **提供元:** Princeton NLP (HuggingFace `princeton-nlp/SWE-bench_Verified`,
  <https://huggingface.co/datasets/princeton-nlp/SWE-bench_Verified>)。
- **ライセンス:** データセットは上流条件で配布 (SWE-bench ツールは MIT。参照先
  リポジトリは各自のライセンス)。
<!-- /record -->
