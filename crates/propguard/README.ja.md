Stop hook として配線した場合（既定では未配線）、`Stop` のたびに、現在のタスクの `done_criteria` から *semantic property*（不変条件）を導出し、生成コードがそれを満たすかを検査してからターン完了を許可します。

 プロパティの導出は **PGS**（Property-Generated Solver, [arXiv:2506.18315](https://arxiv.org/pdf/2506.18315)）に倣います。

プラグインは既定では Stop hook を配線しない（`hooks/hooks.json` は何も登録しない）。Stop ゲートは下記の `propguard install` で明示的に配線した場合のみ動く。

バイナリはプロパティの *導出* と count→threshold のブロック判定を決定論的に担い、各プロパティが実際に *成り立つか* の意味判断は、subscription 上で動いている当のエージェント（inject モード）か、設定した独立チェッカー（subprocess モード）に委ねます。

導出は bilingual なキーワード分類に基づく決定論的ルールで、`max_properties` 個に上限。

| id | 不変条件 |
|----|----------|
| `error-path` | 失敗パスは panic せず Err/エラーを返す |
| `output-schema` | 出力スキーマ/フォーマットが安定している |
| `determinism` | 決定論的: 同一入力は同一出力を返す |
| `idempotence` | 冪等: 複数回実行しても結果が変わらない |
| `bounds-monotonicity` | 境界・単調性・閾値が守られる |
| `no-partial-write` | 部分書き込みが起きない |

done_criteria にキーワードが現れたプロパティを導出し、`min_properties` に満たなければ *universal*（基礎）不変条件（`error-path`, `output-schema`, `determinism`）で補完します。

オフラインで導出を確認せよ：

```
propguard derive "冪等に再実行でき、失敗時は panic せずエラーを返し、出力スキーマを壊さないこと"
```

done_criteria の取得元は優先順に：

1. 環境変数 `PROPGUARD_CRITERIA`
2. project root の `criteria_file`（既定 `.propguard-criteria`）
3. `propguard.toml` の inline `done_criteria`

いずれも設定されていなければ **すべての停止を許可**。ただし `criteria_file` が存在するのに読めず inline `done_criteria` も空の場合は「未設定」ではなく判定不能として **ブロック**（`criteria-unreadable`）。

| モード | 動作 |
|--------|------|
| `inject` | 新しい diff ごとに 1 回ブロックして **プロパティ・チェックリスト** を注入する。propguard は各ファイルの作成者を検証していないため、検査対象には人間や別セッションの変更も含まれうる。 |
| `subprocess` | `checker_cmd`（既定 `claude -p`） を起動し、プロパティごとに `PROP <id>: PASS\|FAIL` を 1 行出力させ、PASS を数える。 |

既定のモードは `inject`。

`gate::below_threshold` で：**`satisfied < threshold` のときブロック**。
inject モードでは新しい diff は *未検証* なので、まずチェックリスト注入のため 1 回ブロックし、次の停止では同じ diff を許可する — *trust-after-one-block*：エージェントが実際にプロパティを検証したかを propguard は観測しておらず、保証するのは「チェックリストを 1 回提示した」ことだけ。
subprocess モードでは PASS 数を直接比較。
閾値は実際に導出されたプロパティ数にクランプされる。

`(diff, properties)` をハッシュ化。
ハッシュが記録されるのは subprocess の合格（`properties-satisfied`）か inject の trust-after-one-block のとき。
記録済みハッシュと一致する停止は許可（`already-verified`）。
subprocess の閾値未満はハッシュを記録しないので次の停止で再検査する。
ブロックした各ラウンドは `max_attempts`（既定 2、`reset_after_secs`〈既定 600〉の無操作でリセット）に数えられる。
超えると閾値未満のまま **許可**（tag `giveup`、プロパティは未充足）。

- git リポジトリでない / 検査対象なし / done_criteria が一切設定されていない → **許可**
- 設定ファイル（`propguard.toml` / `~/.propguard/config.toml`）が存在するのに読めない・解析できない → **ブロック**（`config-unreadable`、自動通過なし）— 組み込み既定値で黙って代用しない
- 解析はできたが `mode` の値が未知 → **ブロック**（`config-invalid`、自動通過なし）— どちらのモードにも写さない
- `criteria_file` が存在するのに読めず inline も空 → **ブロック**（`criteria-unreadable`、自動通過なし）
- `PROPGUARD_CRITERIA` が設定されているのに UTF-8 でない → **ブロック**（`criteria-unreadable`、file / inline へフォールバックしない）
- `checker-error-giveup` は overwatch の violation ledger に記録し、セッションを跨いで再発したら **ブロック**（`checker-outage-systemic`）、ledger が読めなければ **ブロック**（`checker-outage-undetermined`）。
  エスカレーションの mapper（`gate::escalate_giveup_on_systemic`）は systemic な `git-scan-failed-giveup` / `diff-read-failed-giveup` もブロックに写す。
  `truncated-giveup` は障害ではないので対象外
- チェッカーが crash / timeout / 解析不能出力 → **ブロック**（有界）後に通過
- 実リポジトリ内で `git` コマンドが失敗（変更集合が判定不能）→ **ブロック**（有界、`git-scan-failed`）後に通過（`git-scan-failed-giveup`）
- 大きすぎて切り詰められた diff（未検査の末尾）→ **ブロック**（有界）後に通過
- diff が **読めなかった** — `git diff` / `git diff --cached` / untracked の `ls-files` / untracked ファイル本体のいずれかがデコードできない → **ブロック**（有界、`diff-read-failed`）後に通過。
  一方が読めて他方が読めない部分的なケースも同じで、読めた半分を diff 全体であるかのように渡さない。
  読めなかった diff は判定不能であって「空 ⇒ 変更なし」ではない
- ハーネス自身の panic → 1 回目の停止で fail-closed に **ブロック**：Stop の入口 `check` は `harness_core::gate::run::run_guarded` を通り、その下の panic は block 判定を出す

共有された project root の `.propguard-skip` ファイルはもう停止を通さない。
エスケープ：`propguard skip --reason "<理由>"`（1 回限り・理由必須・**発行したセッション限定**・発行と消費の両方をゲートログに記録）、設定の `enabled = false`、または `PROPGUARD_DISABLE=1`。

## プラグインをインストールせよ

```
/plugin marketplace add yukineko/claude-harnesses
/plugin install propguard@yukineko
```

## ソースからインストールせよ

```
cargo build --release -p propguard
./target/release/propguard install     # Stop hook を ~/.claude/settings.json に配線
propguard init                         # 雛形 ./propguard.toml を生成
```

`propguard uninstall` は Stop hook を除去する。
`propguard --protects` はこのゲートが何を・何から・なぜ守るかを表示する。
セッション状態は `~/.propguard/state`（設定キー `state_dir`、環境変数 `PROPGUARD_STATE_DIR` で上書き可）。

[`propguard.example.toml`](./propguard.example.toml) を参照。
主なつまみ：`mode`、`min_properties`/`max_properties`（`max_properties` は 5 に上限、`min_properties` = 1 は 3 へ引き上げずそのまま受け付ける）、`threshold`、`max_attempts`、`reset_after_secs`、`min_changed_files`、`max_diff_bytes`、`criteria_file`、`include`/`exclude`、`enabled`、（subprocess）`checker_cmd` / `checker_timeout_secs`。
project の `propguard.toml` は root を **trust** して初めて honored される。
root は `propguard trust` で trust する。

`propguard status` で解決済み設定と現タスクのプロパティを確認できる。

`subprocess` モードでは `checker_cmd` は実 OS subprocess として起動され、タイムアウトで上限が付く：

- **既定 300 秒**。
- `propguard.toml`（または `~/.propguard/config.toml`）のトップレベルキー `checker_timeout_secs` で設定：

  ```toml
  checker_cmd = "claude -p"
  checker_timeout_secs = 45   # 既定 300 秒を上書き
  ```

- `0` はタイムアウト無効化ではなく既定 300 秒へ sanitize される。
  不正値・負値は TOML 解析に失敗し、設定全体が読めない扱いで **ブロック**（`config-unreadable`）になる（黙って既定値にはならない）。
- **タイムアウト時はチェッカーのバックグラウンド化された子孫も kill する**：シェル経由の `checker_cmd` が `&` でバックグラウンド化した孫はタイムアウト後に生き残らない。
  stdout パイプを開いたままの子孫がいても停止はハングしない。
- タイムアウトはチェッカーの `Error` として報告され（「全件合格」には決してならない）、他のチェッカー失敗と同じ fail-closed・`max_attempts` で有界な give-up 経路を通る
