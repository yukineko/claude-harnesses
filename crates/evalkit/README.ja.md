# evalkit

harness モノレポ向けのオフライン **ゴールデン回帰テスト harness**。condukt のオンライン Phase-6 検証器の*オフラインの兄弟*にあたる。

## 目的

evalkit は、ゴールデンとして固定した `*.jsonl` ケースを読み込み、各 subject に対して決定論的にアサートし、不変条件が回帰したら非ゼロで終了する harness である。**API キー不要**で動くため、CI ゲートとしても `/flow` のリリース前チェックとしても回せる。

1 ケースは 1 つの *subject* を指す。subject は `file`（そのファイルの内容）または `cmd`（その標準出力）のどちらか一方であり、それに対してアサーションを並べる。

```jsonl
// プロンプト不変条件: flow はハードルールを保持し続けなければならない
{"id":"flow-keeps-blind-exec","file":"crates/flow/skills/flow/SKILL.md","assert":{"contains":["盲目実行しない"]}}
// CLI 契約: compass nudge は機械可読な判定を出す
{"id":"compass-nudge-json","cmd":["compass","nudge","--json"],"assert":{"exit":0,"regex":["\"fresh\"\\s*:\\s*(true|false)"]}}
```

主なケースのフィールドは次の通り。

| フィールド | 意味 |
|---|---|
| `id` | 安定したケース名（必須） |
| `describe` | 人間向けの 1 行ラベル（任意） |
| `file` | このファイルの内容を subject として読む（`--root` からの相対パス） |
| `cmd` | `cmd[0]` を残りを引数として実行し、標準出力を subject として捕捉する |
| `stdin` | `cmd` subject に流す標準入力（任意） |
| `assert.exit` | 期待する終了コード（`cmd` のみ） |
| `assert.contains` / `not_contains` | 現れる／現れてはならない部分文字列 |
| `assert.regex` / `not_regex` | マッチする／してはならない正規表現 |
| `draft` | 人手のアサーションを待つ昇格ドラフト。実行時はスキップされる（pass/fail どちらにもならない） |

1 ケースは `file` か `cmd` の**ちょうど一方**を持つ——ただし `draft: true` の
ケースはまだ subject を持たないため、この要件を免除される。

## どうして必要か

condukt の検証器は*タスクごと・オンライン*で動く。サブエージェントを起こして新しい diff を判定するので、**新規**作業の回帰は捕まえられる。だが、すでにプラグインへ焼き込まれたガードレール——`SKILL.md` のハードルール、`--json` CLI 契約の形——は誰も再点検しない。

不用意なプロンプト編集が flow のスキルから「`盲目実行しない`」をひっそり削っても、オンライン検証器はそれに気づかない。これが痛点である。

evalkit はこの隙間を塞ぐ。ゴールデンに固定した不変条件を決定論的にアサートし、回帰したら CI を赤くしてマージ前に止める。終了コードは原因を区別できる。

| コード | 意味 |
|---|---|
| `0` | 全ケース通過 |
| `1` | 真の回帰（アサーション失敗） |
| `2` | harness エラー（ケースが見つからない、eval ファイルが読めない等） |

## どう使うか

evalkit は単一の Rust バイナリで、ジョブごとにサブコマンドを公開する。サブスクリプションネイティブで API キーも追加インストールも不要だ。

```sh
evalkit run                                   # ./evals/*.jsonl を探索しアサート、失敗で非ゼロ終了
evalkit run --root . --bin-dir target/release # `cmd` のプログラムを新しいビルドから解決する
evalkit run --json                            # 機械可読なサマリ
evalkit list                                  # ケースを実行せず一覧表示する
```

`--bin-dir DIR` は `cmd` ケースの解決時に `PATH` の先頭へ追加される。これにより、ビルドしたての `target/release/<tool>` をインストールせずに走らせられる。

`draft` ケースは `skip ... (draft — assertion pending)` として報告され、別枠で集計される——pass/fail の終了コードには一切寄与しない。

### canary: 同じゴールデンを 2 バージョンで再生する

`evalkit canary` は 2 つの `evalkit run --json` 出力を差分比較する。同じゴールデン集合を 2 つの地点（PR の base と head、旧 SKILL.md と新 SKILL.md）で再生し、プロンプト編集が挙動を変えたときに*どのゴールデンが動いたか*を見せる。

```sh
evalkit run --json > base.json        # 旧バージョンで
evalkit run --json > head.json        # 新バージョンで
evalkit canary --baseline base.json --current head.json
evalkit canary --baseline base.json --current head.json --json               # 機械可読な差分
evalkit canary --baseline base.json --current head.json --fail-on-regression # pass→fail があれば exit 1
```

ケースを `id` でキーにし、それぞれを **regression**（pass→fail）、**fix**（fail→pass）、**added**、**dropped** に分類し、合格率を before → after と delta で表示する。既定では**情報提供のみ**（exit 0）で、`eval.yml` の `canary` ジョブはゲートせず PR に注釈するだけである。`--fail-on-regression` を渡すと pass→fail が 1 件でもあれば exit 1 のハードゲートになる。

| コード | 意味 |
|---|---|
| `0` | 報告済み（`--fail-on-regression` 時は回帰なし） |
| `1` | `--fail-on-regression` 指定かつ pass→fail が 1 件以上 |
| `2` | harness エラー（結果ファイルが読めない／パースできない） |

### ゴールデンの置き場所と CI

ゴールデンはリポジトリルートの `evals/` にある。

- `evals/skill-invariants.jsonl` — プラグイン `SKILL.md` に固定したハードルール。
- `evals/cli-contracts.jsonl` — CLI の出力／終了コード契約。
- `evals/replay/replayed.jsonl` — 昇格済みの condukt トラジェクトリ再生ケース
  （`evals/replay/fixtures/` 配下のフィクスチャに対して `replaykit verify` を
  アサートする `cmd` ケース）。

新しい不変条件を成文化するたびに 1 行を足す。CI 配線は `.github/workflows/eval.yml` がワークスペースをビルドし、push/PR ごとに `evalkit run --bin-dir target/release` を回す。不変条件が落ちればマージ前にジョブが赤くなる。`/flow` のリリース前ゲートとして `evalkit run` を組み込むこともできる。
