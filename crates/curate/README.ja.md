# curate

**fugu-router の playbook** を、[evalkit](../evalkit) 向けの**バージョン管理された golden eval データセット**へ昇格させる。オフライン eval ループの供給側（supply side）。

## 目的

curate は、fugu-router が検証済みタスクごとに 1 件記録する playbook を、evalkit が消費できる golden ケースへ蒸留し、バージョン管理・重複排除されたデータセットへ固定する CLI である。

fugu-router は検証を通った各タスクについて、`title` / `done_criteria` / `touched_files` を `~/.fugu-router/playbooks.jsonl` に追記する。curate はそこから選んだ 1 件を、evalkit の golden ケース（`input→expected` のテスト）として `evals/curated/<name>.jsonl` に書き出す。

蒸留の方針は **honest mapping**——「playbook は手順（procedure）であってテストそのものではない」という前提に立つ。受け入れ基準（`done_criteria`）が**機械的（mechanical）**なときだけ、実行可能なケースを自動導出する。

| `done_criteria` | 昇格後の golden |
|---|---|
| `` `cargo test --workspace` passes `` | `{"cmd":["cargo","test","--workspace"],"assert":{"exit":0}}` |
| "cargo test -p evalkit is green" | `{"cmd":["cargo","test","-p","evalkit"],"assert":{"exit":0}}` |
| "auth handles token refresh" | `{"draft":true,"describe":"… — TODO assert done_criteria: …"}` |

機械的とみなすシグナルは、明示的にバッククォートで囲まれたコマンド、または認識済みのテストランナー（`cargo test` / `npm test` / `pytest` / `go test`）。それ以外は **draft（下書き）** になる。draft は妥当な golden だが、人間がアサーションを書くまで evalkit が**スキップ**する（pass にも fail にもならない）ため、CI を壊さずに「未対応の保留作業」としてリポジトリに可視化したまま置いておける。

## どうして必要か

evalkit は golden を消費するが、これまで**実際に検証された仕事から golden を生み出す仕組みが存在しなかった**。

fugu-router の playbook ログは、追記専用（append-only）で、policy-search（方策探索）専用だった——holdout として整理されることはなく、`input→expected` のテストも持たない。つまり「検証を通った本物の作業」が蓄積されていても、それが回帰テスト（regression golden）として再利用される経路がなかった。

curate はこのギャップを埋める。選んだ playbook エントリを evalkit golden ケースへ蒸留し、バージョン管理・重複排除されたデータセットに固定することで、検証済みの 1 回の実行を回帰用 golden に変える。これがなければ、せっかく検証を通った成果は追記ログに埋もれたままで、オフライン eval ループの供給側が欠けることになる。

## どう使うか

人間が直接叩く、あるいは condukt の Phase 6（record の後）が提案する**素の CLI** である。ライフサイクル hook ではない。

```sh
curate candidates                            # 昇格可能な playbook を一覧（mech | draft）
curate promote "add login" --dataset auth    # → evals/curated/auth.jsonl
curate promote --latest                      # 直近に記録された playbook を昇格
curate promote "x" --draft                   # 機械的でも強制的に draft にする
curate promote --latest --store /path/to/playbooks.jsonl  # playbook ストアを明示指定
```

主なサブコマンド:

- `curate candidates` — playbook ストア（既定 `~/.fugu-router/playbooks.jsonl`、`--store` で上書き可）を読み、昇格可能なエントリを `mech`（自動でコマンド化できる）/ `draft` 別に一覧する。
- `curate promote` — 1 件の playbook を golden データセットへ昇格する。`title` 部分一致（大文字小文字を無視・最新一致が優先）か `--latest` で選び、`--dataset <name>` で出力先 `evals/curated/<name>.jsonl` を決める。`--draft` で機械的でも draft 化、`--root` で出力先の基準ディレクトリ（既定 CWD）、`--evals-dir`（既定 `evals`）で `curated/` を置くディレクトリ名、`--store` で読み込む playbook ストアのパスを指定する。

昇格はケース id で重複排除しつつ `evals/curated/<name>.jsonl` に追記される。evalkit は `evals/` を**再帰的に**探索するため、昇格したケースは設定変更なしで `evalkit run` および `eval.yml` の CI ゲートに拾われる。

このループの位置づけ:

```
condukt がタスクを検証 ─▶ fugu-router record (playbook)
        ▲                              │
        │                    curate promote
   evalkit run  ◀── evals/curated/*.jsonl ◀┘   (eval.yml が push ごとにゲート)
```

record の後、condukt の Phase 6 は `curate promote` に **HOTL 連携**されている。verified タスクの `done_criteria` が機械的で、かつ `curate` が PATH 上にあるとき、main loop は**ちょうど 1 回**だけ「この verified run を eval golden 化しますか？」と確認し、**肯定回答が返ったときに限り**実際に `curate promote` を shell-out する（否定なら書き込みは一切行わない）。**サブスクリプションネイティブ**——バンドルされた単一の Rust バイナリで完結し、API キーは不要。
