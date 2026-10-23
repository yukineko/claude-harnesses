オフライン **ゴールデン回帰テスト harness**。

ゴールデンとして固定した `*.jsonl` ケースを読み込み、各 subject に対してアサートし、不変条件が回帰したら非ゼロで終了する。

1 ケースは 1 つの *subject* を指し、subject は `file`（そのファイルの内容）または `cmd`（その標準出力）のどちらか一方であり、それに対してアサーションを並べる。

```jsonl
// プロンプト不変条件: flow はハードルールを保持し続けなければならない
{"id":"flow-keeps-blind-exec","file":"crates/flow/skills/flow/SKILL.md","assert":{"contains":["盲目実行しない"]}}
// CLI 契約: compass nudge は機械可読な判定を出す
{"id":"compass-nudge-json","cmd":["compass","nudge","--json"],"assert":{"exit":0,"regex":["\"fresh\"\\s*:\\s*(true|false)"]}}
```

- `id` — 安定したケース名（必須）
- `describe` — 人間向けの 1 行ラベル（任意）
- `file` — このファイルの内容を subject として読む（`--root` からの相対パス）
- `cmd` — `cmd[0]` を残りを引数として実行し、標準出力を subject として捕捉する
- `stdin` — `cmd` subject に流す標準入力（任意）
- `assert.exit` — 期待する終了コード（`cmd` のみ）
- `assert.contains` / `not_contains` — 現れる／現れてはならない部分文字列
- `assert.regex` / `not_regex` — マッチする／してはならない正規表現
- `draft` — 人手のアサーションを待つ昇格ドラフトであり、実行時はスキップされる（pass/fail どちらにもならない）

1 ケースは `file` か `cmd` の**ちょうど一方**を持つ——ただし `draft: true` の
ケースはまだ subject を持たないため、この要件を免除される。

- subject は**標準出力だけ**である。標準エラーにだけ書かれたテキストは subject に含まれない。
- 取得失敗（ファイルが読めない／cmd が spawn できない）はケースの失敗として報告される。
- `file` ケースに `assert.exit` を書くと、それ自体が失敗になる（ファイルに終了コードは無い）。

- `0` — 全ケース通過
- `1` — 真の回帰（アサーション失敗）
- `2` — harness エラー（ケースが見つからない、eval ファイルが読めない等）、または全ケースが draft で何も検査していない場合

```sh
evalkit run
evalkit run --root . --bin-dir target/release
evalkit run --dir evals/replay
evalkit run --json
evalkit list
```

`--dir` は `*.jsonl` を**再帰的に**探索するので、`evals/replay/` のようなサブディレクトリも設定なしで拾われる。`--dir` の既定は `evals` である。`--root`（既定は CWD）は `file` パスと `--dir` の基点であり、`cmd` サブプロセスの cwd でもある。空行と `//` 行は読み飛ばされるが、それ以外のパースできない行や、`id` が空・subject が両方／どちらも無いケースが 1 つでもあると、実行全体が harness エラー（exit 2）になる。`list` も同じフラグを受け付けるが、常に素の一覧を出す。

`--bin-dir DIR` は `cmd` ケースの解決時に `PATH` の先頭へ追加される。

`draft` ケースは `skip <label> (draft — assertion pending)` として報告され、サマリで別枠に集計される（`N draft(s) skipped`）。

`evalkit canary` は 2 つの `evalkit run --json` 出力を差分比較する。

```sh
evalkit run --json > base.json
evalkit run --json > head.json
evalkit canary --baseline base.json --current head.json
evalkit canary --baseline base.json --current head.json --json
evalkit canary --baseline base.json --current head.json --report-only # 入力が使えない場合を除き常に exit 0
```

ケースはレポート上のラベル（`run --json` の `case` フィールド：`id`、`describe` があれば `id — describe`）でキーにする — そのため `describe` を編集すると dropped 1 件 + added 1 件として現れる。それぞれを **regression**（pass→fail、どちらも draft でない）、**fix**（fail→pass）、**added**、**dropped**、**demoted**（pass→draft・fail→draft）に分類し、合格率を before → after と delta で表示する。合格率の分母から draft は除外され、全件 draft または空なら 0.0 になる。既定で canary は**ゲートする**: ゴールデンの判定を 1 件でも失えば exit 1 になる。`--report-only` は情報提供のみ（exit 0）にする明示的な opt-in である。`--report-only` でも exit 2 は和らがない: 全件 draft の側は何も検査していない。旧 `--fail-on-regression` フラグは削除され、渡すと拒否される。

`dropped` は自分で確認すること。

- `0` — ゴールデンの判定を 1 件も失っていない、または `--report-only` 指定
- `1` — ゴールデンの判定を失った: pass→fail の回帰、dropped、pass→draft の demoted、失敗を隠す fail→draft の demoted
- `2` — harness エラー（結果ファイルが読めない／パースできない）、または判定を持つケースが無い側がある（全件 draft・空）

ゴールデンはリポジトリルートの `evals/` にある。

- `evals/skill-invariants.jsonl` — プラグイン `SKILL.md` に固定したハードルール。
- `evals/cli-contracts.jsonl` — CLI の出力／終了コード契約。
- `evals/replay/replayed.jsonl` — 昇格済みの condukt トラジェクトリ再生ケース
  （`evals/replay/fixtures/` 配下のフィクスチャに対して `replaykit verify` を
  アサートする `cmd` ケース）。

新しい不変条件を成文化するたびに 1 行を足すこと。

**CI 配線は無い**: かつての `.github/workflows/eval.yml` は存在しない（このリポジトリは GitHub Actions を使わない）し、evalkit はフックも持たない。ビルド後に自分で `evalkit run --bin-dir target/release` などと実行すること。
