**コードレビュー・ゲート**。

1 本の Stop フックを登録し、それがバンドルされた `reviewgate review` バイナリを実行する。

- `mode` のデフォルトは `off`：設定ファイルが無ければレビューせず、ブロックもルーブリック注入もせずに stop を許可する。レビューは `mode = "inject"` か `mode = "subprocess"` による opt-in。
- `inject` モードでは、セッション内でレビュー対象の変更がある最初の stop をブロックする。
- 注入する理由文はレビュー用の**ルーブリック**をそのまま含み、ファイル一覧を示してレビューを求める。
- 注入する理由文は、一覧を作業ツリーの未コミット変更として記述し、セッション帰属は transcript からの推定であって作成者は厳密には検証していないと明記する。
- `subprocess` モードは `reviewer_cmd`（デフォルト `claude -p`）をレビュアーとして走らせる。
- `subprocess` モードでは、レビュアーが LGTM と答えれば stop を通し、報告された指摘はそのままブロック理由に描画される。

最後にレビューを強制したときと同じ diff の stop は通過させる（ログタグ `already-reviewed`）。
`already-reviewed` で通過した後に diff が変わった場合は再びブロックし、以前のブロックの繰り返しとして waive しない。
ブロックするラウンド数は `max_attempts` で上限がかかるため、エージェントが無限に閉じ込められることはない。inject モードの give-up は最新の diff を未レビューのまま通すため、stderr と `systemMessage` で明示する。
`max_attempts` のデフォルトは 2。

レビュー対象がそもそも存在しない場合（git リポジトリでない・レビュー対象の変更が無い）は stop を**許可**する。
判定できなかった場合は許可しない：git コマンドが実リポジトリ内で失敗した変更集合は *undetermined* であり、**ブロック**する（`max_attempts` で有界）。
同様に、レビュー対象の変更があるのに diff 本文が空で返った場合（`.gitattributes` の diff ドライバが何も出力しない等）も「空の diff」ではなく *undetermined* であり、**ブロック**する（`max_attempts` で有界）。
設定ファイルが読めない／parse できない場合は既定の `off` に落とさず、WARNING を出して `config-invalid` でブロックする。
`review` フックは `harness_core::gate::run::run_guarded` の下で走る：最初の stop での panic はブロック（**fail closed**）し、再入時（`stop_hook_active`）の panic だけが許可へ落ちるので、壊れた reviewgate が turn を永久に塞ぐことはない。
ロックファイルはデフォルトで除外される。
`node_modules`・`target`・生成物はデフォルトで除外される。

**誰の変更か**：レビューするのは**本セッションが書いたファイルだけ**（`harness_core::attribution::attribute_own_writes`、hook 入力の `transcript_path` を読む。tdd と共有）：本セッションの transcript かサブエージェントの sidechain に `Edit`/`Write`/`MultiEdit`/`NotebookEdit` の記録があるファイル、またはそのパスを読み取り専用でない文で名指しした `Bash` コマンドがあるファイルを残し（`git diff`・`grep`・`cat` などで読むだけなら帰属しない）、それ以外の変更ファイルは本セッションに帰属しないものとして除外する。
既知の穴: パスを名指ししないコマンド（`cargo fmt`・シェルの glob・スクリプト）で書いたファイルも除外される。
その除外はブロック理由に追記される。
その除外は stderr に出力される。
帰属が判定不能（transcript が無い／読めない）なら変更ファイルを全件レビューする。

*レビュアー*自体の失敗は「レビュー結果クリーン」とは**異なる**ため、無言で許可はしない（壊れたレビュアーがバイパスになってしまうため）：

- レビュアーの subprocess が起動できない / timeout / 解析不能な出力（stdout が空・非 UTF-8）の場合 → **ブロック**（`max_attempts` で有界）。
- レビュアーに prompt を渡せなかった場合 → **ブロック**。
- diff が大きすぎて丸ごとレビューできず切り詰められた（`max_diff_bytes` で truncate）場合、未レビューの末尾が残る → **ブロック**（`max_attempts` で有界）。
- diff 内容の取得が一部でも失敗した（`git diff` / `git ls-files` のエラー、未追跡ファイルが読めない）場合、diff は不完全 → **ブロック**（変更スキャン失敗と同じ有界経路）。
- 独立レビュアーが `max_attempts` 回を超えても所見を返し続けた場合は通過させるが、stderr に WARNING を出し、専用タグ `review-giveup` で記録し、overwatch の violation event も残す。
- `include` の glob が 1 つでも不正でもレビュー対象は狭まらず、stderr にそのパターンが示される。
- `exclude` の不正 glob は何も除外せず、stderr に WARNING を出す。
- 設定ファイルが読めない／parse できない場合は stderr に WARNING を出し、`status` に `FAILED to load` と表示する。

切り詰めのブロック理由は `reviewgate skip --reason "<理由>"`・`REVIEWGATE_DISABLE=1`・`max_diff_bytes` の引き上げを示す。レビュアー指摘のブロック理由は `reviewgate skip --reason` と `REVIEWGATE_DISABLE=1` を示す。

```
/plugin marketplace add yukineko/claude-harnesses
/plugin install reviewgate@yukineko
```

```
cargo install --path .
reviewgate init          # 雛形の ./reviewgate.toml を書き出す
reviewgate install       # Stop フックを ~/.claude/settings.json に配線する
```

- `reviewgate review` — Stop フック本体（`session_id` を含むフック JSON を stdin から読む）。
- `reviewgate install [--dry-run]` / `uninstall [--dry-run]` — フックの配線を管理する。
- `reviewgate init [--force]` — 雛形の `reviewgate.toml` を書き出す。
- `reviewgate status` — 解決済みの設定と、いま何がレビュー対象になるかを表示する。
- `reviewgate trust` — 現在のプロジェクトを信頼し、その `./reviewgate.toml`（`reviewer_cmd` を含む）を honored にする。
- 信頼するまで、リポジトリ同梱の設定は無視される。
- `reviewgate skip --reason "<理由>"` — 一度だけ・セッション限定のスキップ（下記参照）。

`reviewgate review` を stdin 無しで手実行するとドライチェックになり、ブロック時は exit 1 でフック JSON を出さない。

[`reviewgate.example.toml`](reviewgate.example.toml) を参照。
プロジェクトの `./reviewgate.toml` が `~/.reviewgate/config.toml` より、それが組み込みデフォルトより優先される（ただし `reviewer_cmd` を subprocess 実行するため、project root を **trust**（`reviewgate trust`）して初めて project 設定が honored される）。

主なフィールド：`mode`、`max_attempts`、`reset_after_secs`（既定 600、セッションごとの試行カウンタがこの無操作秒数でリセットされる）、`min_changed_files`、`max_diff_bytes`（既定 200000）、`include`/`exclude` の glob、`rubric`、`state_dir`、そして（subprocess 用の）`reviewer_cmd` / `reviewer_timeout_secs`。
`max_attempts` / `min_changed_files` の `0` は 1 に clamp され、`max_diff_bytes` の `0` は既定値に戻る。

`mode` に指定できるのは `"off"`・`"inject"`・`"subprocess"`（別名 `reviewer` / `independent`）。
それ以外の値は **config-invalid** で、どちらのモードにも推測で割り当てず、（レビュー対象が無い場合も含め）stop を `config-invalid`（値を明示）でブロックする。
状態は `reviewgate status` で確認でき、抜け道は `reviewgate skip --reason ...` か `REVIEWGATE_DISABLE=1`。

プラグインは `agents/reviewgate-typing.md` も同梱する：tools に Read を含み Edit・Write を含まない subagent 定義である。
`review` フックはこれを呼ばないので、明示的に呼び出して使う。

- 一度だけ：`reviewgate skip --reason "<理由>"`。
- 理由は必須。
- スキップは**発行したセッションにだけ**適用される。
- スキップは一度消費されて次の stop を許可する。
- 発行と消費の両方がゲートログに記録される。
- 旧来の project root の `.reviewgate-skip` ファイルは撤去した：もう stop を素通りさせない。
- 完全に無効化：設定で `enabled = false`、または `REVIEWGATE_DISABLE=1`。

各判定は `<state_dir>/log.jsonl`（デフォルトは `~/.reviewgate/state/log.jsonl`）に JSONL 1 行として追記される。

License: MIT
