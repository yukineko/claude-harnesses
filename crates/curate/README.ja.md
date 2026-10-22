`curate promote` は選んだ playbook エントリ（`title` と `done_criteria` を持つ JSON 行）を、`id` 付きの golden ケース行に変換する。

curate は受け入れ基準（`done_criteria`）が**機械的（mechanical）**なときだけ、実行可能なケースを自動導出する。それ以外は draft になる。

- `` `cargo test --workspace` passes `` は `"cmd":["cargo","test","--workspace"]` と `"assert":{"exit":0}` を持つ。
- "cargo test -p evalkit is green" は `"cmd":["cargo","test","-p","evalkit"]` と `"assert":{"exit":0}` を持つ。
- "auth handles token refresh" は `{"draft":true,"describe":"… — TODO assert done_criteria: …"}` になる。

機械的とみなすシグナルは、明示的にバッククォートで囲まれたコマンド、または認識済みのテストランナー（`cargo test`、`npm test` と `pytest`）。`go test` も認識済みのテストランナーである。

evalkit は draft ケースをスキップする（pass にも fail にもならない）。

```sh
curate candidates                            # 昇格可能な playbook を新しい順に一覧（mech | draft）; --k N で件数を絞る（既定 20）
curate promote "add login" --dataset auth    # → evals/curated/auth.jsonl
curate promote --latest                      # 直近に記録された playbook を昇格
curate promote "x" --draft                   # 機械的でも強制的に draft にする
curate promote --latest --store /path/to/playbooks.jsonl  # playbook ストアを明示指定
```

`promote` は `--store` で別パスの playbook ストアを読める。`candidates` も `--store` で指定したパスの playbook ストアを読む。既定のストアパスは `~/.fugu-router/playbooks.jsonl` である。

`promote` は `title` の部分一致（大文字小文字を無視）で選び、最新の一致が優先される。`promote --latest` は最新の playbook を選ぶ。

昇格は `<root>/<evals-dir>/curated/<name>.jsonl` に追記され、`--evals-dir` の既定は `evals` である。`--dataset` の既定は `promoted` である。昇格はケース id で重複排除される。`--root` の既定は CWD である。

evalkit は `*.jsonl` の golden を再帰的に探索するため、`curated/` 配下のケースも見つかる。
