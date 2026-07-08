# benchkit

harness モノレポ用の**外部ベンチマークランナー**。対象は **SWE-bench Verified**。
本スライスはスケルトン: 型付き instance モデル、決定論的な fixture ベース JSONL
ローダー、そして gate 付き `download` サブコマンド。scorer / dashboard / harness
層は後続タスクで追加する。

## なぜ

condukt / evalkit は *自前の* invariant に対してハーネスを測る。benchkit は
*業界標準の外部ベンチマーク* SWE-bench Verified に対して測るので、他エージェントが
公表する数値と比較できる。ローディング経路は純粋・決定論的 (ネットワーク無し・
時刻無し) でテストは hermetic。ネットワークに触れるのは `download` のみ、しかも
明示的に呼んだときだけ。

## Instance モデル

1 つの `Instance` = 1 ベンチマークタスク: `base_commit` に固定した repo、それを
直す gold `patch`、採点テストを足す `test_patch`、候補を採点する 2 つのテスト集合。

| フィールド | 意味 |
|---|---|
| `instance_id` | 一意な安定 ID (例 `astropy__astropy-12907`) |
| `repo` | 対象 GitHub repo の `owner/name` |
| `base_commit` | パッチ適用前に checkout する commit |
| `patch` | gold 解パッチ (unified diff) |
| `test_patch` | 採点テストを導入/更新するパッチ |
| `problem_statement` | 自然言語の課題 |
| `hints_text` | 任意のヒント |
| `created_at` | 上流タイムスタンプ (原文) |
| `version` | プロジェクトのバージョンラベル |
| `fail_to_pass` | red→green にすべきテスト (`FAIL_TO_PASS`) |
| `pass_to_pass` | green のまま保つべきテスト (`PASS_TO_PASS`) |
| `environment_setup_commit` | 環境/依存を用意する対象 commit |

上流 JSONL は大文字キー `FAIL_TO_PASS` / `PASS_TO_PASS` を使う。モデルは serde
の `rename` で snake_case Rust フィールドへ写す。正規化後 (=`download` が生成し、
fixture が使う形) はこれらを素の文字列 JSON リストで持つ。

## 使い方

```sh
# JSONL split を型付き instance に読む (決定論・オフライン):
benchkit load crates/benchkit/tests/fixtures/instances.jsonl

# 実データをローカルキャッシュへ取得 (冪等・ネットワークはここだけ):
benchkit download                 # → .benchkit-cache/swe-bench-verified.jsonl
benchkit download --dest data/verified.jsonl
benchkit download --force         # キャッシュがあっても再取得
```

`download` は **gate 付き**: ネットワーク経路は明示呼び出し時のみ到達し、
キャッシュ存在時は no-op (冪等)。`cargo test` 中には決して走らない。beacon の
ハウスパターンに倣い、HTTP クライアントを組み込まず `curl` にハードタイムアウト
付きで shell out し、バイナリを小さく保つ。

## データセット出典・ライセンス

- **データセット:** SWE-bench Verified — SWE-bench の人手検証済み 500 タスク部分集合。
- **提供元:** Princeton NLP (HuggingFace `princeton-nlp/SWE-bench_Verified`,
  <https://huggingface.co/datasets/princeton-nlp/SWE-bench_Verified>)。
- **ライセンス:** データセットは上流条件で配布 (SWE-bench ツールは MIT。参照先
  リポジトリは各自のライセンス)。benchkit は明示要求時に公開 split を *取得* する
  だけで再配布しない。`tests/fixtures/` の vendored fixture はオフラインテスト用に
  手書きした極小サンプルであり、データセットの複製ではない。

## 決定論

ローダーは純粋: 同じファイル → 同じ `Vec<Instance>` (ファイル順)、不正行は
`path:line` 付きの明確なエラー。`download` 以外はネットワーク/環境 I/O をしない。
