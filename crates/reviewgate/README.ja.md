# reviewgate

> 🌐 [English](README.md) ・ **日本語**

Claude Code 向けの **コードレビュー・ゲート**。`Stop` のたびに、エージェントがターン完了を宣言する前に diff をレビューする。「これは*動く*か？」を見る [donegate](https://github.com/yukineko/donegate) に対し、reviewgate は「これは*良い*コードか？」を補完する。

## 目的

reviewgate は、エージェントが「終わった」と宣言する直前に、その diff をコードレビューにかける Stop フックである。

1 本の Stop フックとバンドルされた Rust バイナリだけで動くサブスクリプションネイティブな設計で、**API キーは不要**。バイナリは決定論的なオーケストレーターに徹し、LLM による判断は二つのモードのいずれかで行う。

| モード | 何をするか | 独立性 | コスト |
|--------|-----------|--------|--------|
| `inject`（デフォルト） | 新しい diff の状態ごとに一度だけ stop をブロックし、レビュー用の**ルーブリック**を注入する。実行中のエージェントが作業ツリーの未コミット変更をレビューし、完了前に問題を直す（一覧の作成者は検証していない）。 | 自己レビュー | 無料（追加プロセスなし） |
| `subprocess` | `reviewer_cmd`（デフォルト `claude -p`）を**独立した**レビュアーとして diff に対して走らせ、問題が報告されたときだけブロックし、その指摘だけを注入する。 | 独立レビュアー | 1 ラウンドにつき headless レビュー 1 回 |

### どう収束するか

reviewgate はレビュー対象の diff をハッシュ化する。最後にレビューを強制したときと同じ diff の stop は通過させる（エージェントはまさにその diff を既にレビュー済みだから）。diff が*変わった*場合は 1 ラウンド追加されるが、`max_attempts`（デフォルト 2）で上限がかかるため、エージェントが無限に閉じ込められることはない。レビュー対象がそもそも存在しない場合（git リポジトリでない・レビュー対象の変更が無い・diff が空）は stop を**許可**する。一方、判定できなかった場合は許可しない：git コマンドが実リポジトリ内で失敗した変更集合は *undetermined* であり、**ブロック**する（`max_attempts` で有界）。reviewgate 自身が panic した場合も `harness_core::gate::run::run_guarded` が **fail closed** でブロックしクラッシュを表面化させる（連続 2 回目の panic だけが `stop_hook_active` により有界に許可へ落ちるので、壊れた reviewgate が turn を永久に塞ぐことはない）。CLAUDE.md §3 — 「判定できなかった」は「問題なし」ではない。

デフォルトで安全：git リポジトリでない、あるいはレビュー対象のファイル変更が無い場合は stop を許可する。ロックファイル・`node_modules`・`target`・生成物などは除外される。

### fail closed だが有界

*レビュアー*自体の失敗は「レビュー結果クリーン」とは**異なる**ため、無言で許可はしない（壊れたレビュアーがバイパスになってしまうため）：

- レビュアーの subprocess が crash / timeout / 解析不能な出力 → **ブロック**（`max_attempts` で有界）後に警告して通過。
- diff が大きすぎて丸ごとレビューできず切り詰められた（`max_diff_bytes` で truncate）場合、未レビューの末尾が残る → **ブロック**（`max_attempts` で有界）後に警告して通過。

どちらの場合もブロック理由にすべての抜け道（`reviewgate skip --reason "<理由>"`、`REVIEWGATE_DISABLE=1`、`max_diff_bytes` の引き上げ）が明示されるため、壊れたレビュアーや大きすぎる diff が turn を永久に塞ぐことはない。

## どうして必要か

エージェントは「動くコードを書いたら完了」と判断しがちで、コードの質（重複・読みにくさ・抜けたエラー処理など）を自分で見直さないまま turn を閉じてしまう。レビューを人間が後追いで行うと、見落としや手戻りが増える。

reviewgate は、その「完了宣言の瞬間」をレビューのトリガーにする。

- **自己レビューの抜けを塞ぐ。** `inject` モードは、エージェントが diff を見直さずに止まろうとした瞬間にルーブリックを差し込み、作業ツリーの未コミット変更をレビューして直すまで完了させない。追加プロセスもコストもかからない。
- **独立した視点が欲しいときに使える。** `subprocess` モードは別のレビュアーを diff に対して走らせ、その指摘だけをフィードバックする。実装したエージェント自身のバイアスから切り離してレビューできる。
- **無限ループにしない。** diff ハッシュと `max_attempts` により、同じ diff の再レビューは要求せず、変更があっても上限付きで打ち切る。判断は LLM、収束制御は決定論的バイナリ、と役割を分けている。
- **判定できないときは通さない。ただし有界。** そもそもレビュー対象が無いケースは許可するが、git スキャン失敗・レビュアーの異常・diff の切り詰め・自身の panic はいずれも「レビュー結果クリーン」ではないので**ブロック**する。すべて `max_attempts` で有界かつ抜け道が明示されるため、reviewgate が開発を永久に塞ぐことはない。

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

- 一度だけ：`reviewgate skip --reason "<理由>"`。理由は必須で、**発行したセッションにだけ**適用され、一度消費されて次の stop を許可し、発行と消費の両方がゲートログに記録される。（旧来の project root の `.reviewgate-skip` ファイルは撤去した。共有ツリーの 1 回限りマーカーは次に停止したセッションが消費し、その別セッションの正当なゲートを素通りさせるため — CLAUDE.md §5。）
- 完全に無効化：設定で `enabled = false`、または `REVIEWGATE_DISABLE=1`（env は **Claude Code 自身を起動した環境**でのみ有効。フックはアプリの環境を継承するため、ツール呼び出しからの export は届かない）。

### ログ

各判定は `<state_dir>/log.jsonl`（デフォルトは `~/.reviewgate/state/log.jsonl`）に JSONL 1 行として追記される。

## ライセンス

MIT
