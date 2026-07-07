# reviewgate

> 🌐 [English](README.md) ・ **日本語**

Claude Code 向けの **コードレビュー・ゲート**。`Stop` のたびに、エージェントがターン完了を宣言する前に diff をレビューする。「これは*動く*か？」を見る [donegate](https://github.com/yukineko/donegate) に対し、reviewgate は「これは*良い*コードか？」を補完する。

## 目的

reviewgate は、エージェントが「終わった」と宣言する直前に、その diff をコードレビューにかける Stop フックである。

1 本の Stop フックとバンドルされた Rust バイナリだけで動くサブスクリプションネイティブな設計で、**API キーは不要**。バイナリは決定論的なオーケストレーターに徹し、LLM による判断は二つのモードのいずれかで行う。

| モード | 何をするか | 独立性 | コスト |
|--------|-----------|--------|--------|
| `inject`（デフォルト） | 新しい diff の状態ごとに一度だけ stop をブロックし、レビュー用の**ルーブリック**を注入する。実行中のエージェントが自分の変更をレビューし、完了前に問題を直す。 | 自己レビュー | 無料（追加プロセスなし） |
| `subprocess` | `reviewer_cmd`（デフォルト `claude -p`）を**独立した**レビュアーとして diff に対して走らせ、問題が報告されたときだけブロックし、その指摘だけを注入する。 | 独立レビュアー | 1 ラウンドにつき headless レビュー 1 回 |

### どう収束するか

reviewgate はレビュー対象の diff をハッシュ化する。最後にレビューを強制したときと同じ diff の stop は通過させる（エージェントはまさにその diff を既にレビュー済みだから）。diff が*変わった*場合は 1 ラウンド追加されるが、`max_attempts`（デフォルト 2）で上限がかかるため、エージェントが無限に閉じ込められることはない。純粋な*ハーネス*側のエラー（git が無い・設定不正・reviewgate 自身のバグ）は常に stop を**許可**する（reviewgate 自身が壊れて turn を塞ぐことは決してない）。

デフォルトで安全：git リポジトリでない、あるいはレビュー対象のファイル変更が無い場合は stop を許可する。ロックファイル・`node_modules`・`target`・生成物などは除外される。

### fail closed だが有界

*レビュアー*自体の失敗は「レビュー結果クリーン」とは**異なる**ため、無言で許可はしない（壊れたレビュアーがバイパスになってしまうため）：

- レビュアーの subprocess が crash / timeout / 解析不能な出力 → **ブロック**（`max_attempts` で有界）後に警告して通過。
- diff が大きすぎて丸ごとレビューできず切り詰められた（`max_diff_bytes` で truncate）場合、未レビューの末尾が残る → **ブロック**（`max_attempts` で有界）後に警告して通過。

どちらの場合もブロック理由にすべての抜け道（`.reviewgate-skip`、`REVIEWGATE_DISABLE=1`、`max_diff_bytes` の引き上げ）が明示されるため、壊れたレビュアーや大きすぎる diff が turn を永久に塞ぐことはない。

## どうして必要か

エージェントは「動くコードを書いたら完了」と判断しがちで、コードの質（重複・読みにくさ・抜けたエラー処理など）を自分で見直さないまま turn を閉じてしまう。レビューを人間が後追いで行うと、見落としや手戻りが増える。

reviewgate は、その「完了宣言の瞬間」をレビューのトリガーにする。

- **自己レビューの抜けを塞ぐ。** `inject` モードは、エージェントが diff を見直さずに止まろうとした瞬間にルーブリックを差し込み、自分の変更をレビューして直すまで完了させない。追加プロセスもコストもかからない。
- **独立した視点が欲しいときに使える。** `subprocess` モードは別のレビュアーを diff に対して走らせ、その指摘だけをフィードバックする。実装したエージェント自身のバイアスから切り離してレビューできる。
- **無限ループにしない。** diff ハッシュと `max_attempts` により、同じ diff の再レビューは要求せず、変更があっても上限付きで打ち切る。判断は LLM、収束制御は決定論的バイナリ、と役割を分けている。
- **壊れても止めない。** ハーネス側の異常や、そもそもレビュー対象が無いケースでは stop を許可するため、reviewgate 自体が開発を塞ぐことはない。

## どう使うか

### 導入

#### プラグインとして（サブスクリプション、ビルド不要）

```
/plugin marketplace add yukineko/claude-harnesses
/plugin install reviewgate@yukineko
```

#### ソースから

```
cargo install --path .
reviewgate init          # 雛形の ./reviewgate.toml を書き出す
reviewgate install       # Stop フックを ~/.claude/settings.json に配線する
```

### サブコマンド

- `reviewgate review` — Stop フック本体（フック JSON を stdin から読む）。
- `reviewgate install [--dry-run]` / `uninstall [--dry-run]` — フックの配線を管理する。
- `reviewgate init [--force]` — 雛形の `reviewgate.toml` を書き出す。
- `reviewgate status` — 解決済みの設定と、いま何がレビュー対象になるかを表示する。
- `reviewgate trust` — 現在のプロジェクトを信頼し、その `./reviewgate.toml`（`reviewer_cmd` 含む）を honored にする。信頼するまで、リポジトリ同梱の設定は無視される。

`reviewgate review` を stdin 無しで手実行すると、人間向けのドライチェックになる。

### 設定

[`reviewgate.example.toml`](reviewgate.example.toml) を参照。プロジェクトの `./reviewgate.toml` が `~/.reviewgate/config.toml` より、それが組み込みデフォルトより優先される（ただし `reviewer_cmd` を subprocess 実行するため、project root を **trust**（`reviewgate trust`）して初めて project 設定が honored される）。

主なフィールド：`mode`、`max_attempts`、`min_changed_files`、`include`/`exclude` の glob、`rubric`、そして（subprocess 用の）`reviewer_cmd` / `reviewer_timeout_secs`。

### 抜け道

- 一度だけ：プロジェクトルートに `.reviewgate-skip` を作る（1 行の理由を書く）。一度消費され、次の stop は許可される。
- 完全に無効化：`REVIEWGATE_DISABLE=1`、または設定で `enabled = false`。

### ログ

各判定は `<state_dir>/log.jsonl`（デフォルトは `~/.reviewgate/state/log.jsonl`）に JSONL 1 行として追記される。

## ライセンス

MIT
