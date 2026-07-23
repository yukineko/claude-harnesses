# propguard

Claude Code 用の **property gate**（プロパティゲート）。`Stop` のたびに、現在のタスクの
`done_criteria` から 3〜5 個の *semantic property*（不変条件）を導出し、生成コードがそれを
満たすかを検査してからターン完了を許可します。[tdd](https://github.com/yukineko/tdd) の
「具体テストが通るか」に対する「不変条件を保っているか」の相補ゲートです。

`tdd` は具体的なテストケースを走らせますが、コードが満たすべき *意味的* 不変条件を形式化は
しません。**PGS**（Property-Generated Solver,
[arXiv:2506.18315](https://arxiv.org/pdf/2506.18315)）に倣い、propguard は自由記述の
done_criteria を小さな検査可能プロパティ集合に変換し、閾値未満しか成り立たないときブロック
します。

subscription-native：Stop hook 1 本と同梱 Rust バイナリのみ、**API キー不要**。バイナリは
プロパティの *導出* と count→threshold のブロック判定を決定論的に担い、各プロパティが実際に
*成り立つか* の意味判断は、subscription 上で動いている当のエージェント（inject モード）か、
設定した独立チェッカー（subprocess モード）に委ねます。

## 導出されるプロパティ

導出は bilingual なキーワード分類に基づく決定論的ルールで、3〜5 個に上限。カタログ：

| id | 不変条件 |
|----|----------|
| `error-path` | 失敗パスは panic せず Err/エラーを返す |
| `output-schema` | 出力スキーマ/フォーマットが安定している |
| `determinism` | 決定論的: 同一入力は同一出力 |
| `idempotence` | 冪等: 複数回実行しても結果が変わらない |
| `bounds-monotonicity` | 境界・単調性・閾値が守られる |
| `no-partial-write` | 部分書き込みが起きない (atomic) |

done_criteria にキーワードが現れたプロパティを優先して並べ、`min_properties` に満たなければ
どんな生成コードも満たすべき *universal*（基礎）不変条件（`error-path`, `output-schema`,
`determinism`）で補完します。オフライン確認：

```
propguard derive "冪等に再実行でき、失敗時は panic せずエラーを返し、出力スキーマを壊さないこと"
```

## done_criteria の取得元

優先順に：

1. 環境変数 `PROPGUARD_CRITERIA`
2. project root の `criteria_file`（既定 `.propguard-criteria`）— condukt / エージェントが
   現タスクの done_criteria を書き出す
3. `propguard.toml` の inline `done_criteria`

いずれも見つからなければ **すべての停止を許可**（勝手に指摘を作らない）。

## 2 つのモード

| モード | 動作 | コスト |
|--------|------|--------|
| `inject`（既定） | 新しい diff ごとに 1 回ブロックして **プロパティ・チェックリスト** を注入。動いているエージェントが自分のコードを各プロパティで自己検証し、満たさないものを直してから完了。 | 無料（追加プロセス無し） |
| `subprocess` | `checker_cmd`（既定 `claude -p`）を独立チェッカーとして起動。プロパティごとに `PROP <id>: PASS\|FAIL` を 1 行出力させ、PASS を数える。 | 1 ラウンドあたり 1 回の headless チェック |

## ブロック閾値

唯一の判定点（`gate::below_threshold`）：**`satisfied < threshold` のときブロック**。inject
モードでは新しい diff は *未検証*（`satisfied = 0`）なので、まずチェックリスト注入のため 1 回
ブロックし、エージェントが対応した後は同じ diff（プロパティ集合とあわせてハッシュ）を許可。
subprocess モードでは PASS 数を直接比較。閾値は実際に導出されたプロパティ数にクランプされる
ため、恒久的に達成不能になることはありません。

## 収束と安全性

`(diff, properties)` をハッシュ化。直前に検査を強制した停止と一致すれば許可（既に対応済み）。
diff が *変化* すれば 1 ラウンド消費、`max_attempts`（既定 2）で上限。fail-closed だが有界：

- git リポジトリでない / 検査対象なし / done_criteria なし → **許可**
- チェッカーが crash / timeout / 解析不能出力 → **ブロック**（有界）後に警告して通過 —
  壊れたチェッカーはバイパスにならない
- 大きすぎて切り詰められた diff（未検査の末尾）→ **ブロック**（有界）後に通過
- ハーネス自身の panic → `run_guarded` 経由で fail-closed。1 回目の panic は判定不能として
  **ブロック**（クラッシュを表面化）、`stop_hook_active` が立つ 2 回連続 panic のときだけ
  bounded に許可（同じ panic で永久にブロックし続けないための上限）

エスケープ：`.propguard-skip`（1 回限り・理由 1 行）を作成、または `PROPGUARD_DISABLE=1`。

## インストール

### プラグインとして（subscription、ビルド不要）

```
/plugin marketplace add yukineko/claude-harnesses
/plugin install propguard@yukineko
```

### ソースから

```
cargo build --release -p propguard
./target/release/propguard install     # Stop hook を ~/.claude/settings.json に配線
propguard init                         # 雛形 ./propguard.toml を生成
```

## 設定

[`propguard.example.toml`](./propguard.example.toml) を参照。主なつまみ：`mode`、
`min_properties`/`max_properties`（3〜5）、`threshold`、`max_attempts`、`criteria_file`、
`include`/`exclude`、（subprocess）`checker_cmd`。project の `propguard.toml` は root を
**trust**（`propguard trust`）して初めて honored されます（`checker_cmd` を subprocess 実行
するため）。

`propguard status` で解決済み設定と現タスクのプロパティを確認できます。
