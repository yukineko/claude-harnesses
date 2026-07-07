# tdd

Claude Code 向けの test-first ゲート。Rust 製の決定論バイナリ。

## 目的

tdd は、Claude Code の実装作業に対して「テストを先に書く（test-first）」を**強制**し、かつその事実を**検証可能**にする harness である。二つの面で働く。

- **`tdd gate`（Stop hook）**: ターンが止まるたびに git の差分を見て、テストを伴わずに実装コードが追加されていたらその停止をブロックする。理由をエージェントに返すので、エージェントはテストを書いて続行する。
- **`tdd red` / `green` / `verify`（`/tdd` skill が駆動）**: test-first を証跡として記録する。`red` はテストがまず**失敗する**ことを要求して RED 証跡を残し、`green` は先行する RED 証跡を要求したうえでテストが**通る**ことを要求し、`verify` は RED→GREEN が実際に起きたことを確認する。

判断（API 設計・テスト記述・実装）は LLM（=`/tdd` skill）が担い、決定論（RED だったか・RED→GREEN になったか・テスト無し実装のブロック）は `tdd` バイナリと Stop hook が担う。`tdd` は `git` を読みテストコマンドを起動するだけの単一 Rust 実行ファイルで、API キーは不要。LLM の労力（テストを書く・実装する）は Claude Code のサブスクリプション内で完結する。

harness の他のゲートに対する「test-first の兄弟」にあたる。`precommit-audit` が pre-commit で diff のポリシー適合を静的に問い、`donegate` が Stop で実際にビルド/パスするかを問い、`specguard` が要求時に実装の仕様ドリフトを LLM で問うのに対し、**tdd は「テストが、先に書かれたか？」を Stop／`/tdd` で問う**。

## どうして必要か

エージェントはテストを後回しにしがちで、「テストを先に書いた」という主張は通常ただの自己申告にすぎない。人間の意志だけに頼ると次の失敗モードに陥る。

- **テスト無しで実装が着地する。** 実装だけ書いてテストを伴わないままターンが終わってしまう。`tdd gate` は、追加された実装行（impl-glob のファイルから、テストファイルとインラインのテストマーカーを除いたもの）を数え、テストの証跡（追加された `#[test]` / `def test_` / `func Test…` / `it(...)`、または `tests/` 配下の変更ファイル）を探す。実装が追加されているのにテストが無ければ停止をブロックし、エージェントにテストを書かせる。skill を使っていなくてもこのゲートは効く。
- **「テストを先に書いた」が検証できない。** 後からテストを足しても、それが実装より先だったかは分からない。`tdd red` はテストが既に通る場合は証跡の記録を拒否し（それは test-first ではない）、`tdd green` は先行する RED 証跡が無ければ拒否する。これにより RED→GREEN の順序が捏造できない成果物として残る。
- **詰まったエージェントを罠にかけない。** ゲートが連続でブロックし続けるとエージェントが進めなくなる。セッションごとの試行カウンタが `max_attempts` で諦めて停止を許す。純粋なリファクタ/リネーム/ドキュメントには、プロジェクト直下に 1 行の `.tdd-skip` ファイル（1 回だけ消費される）を置く逃げ道があり、`TDD_DISABLE=1` がキルスイッチになる。
- **harness 自身のバグでターンを壊さない。** tdd 側のエラー（自前のバグ、git が読めない等）では hook モードで exit 0 を返し、停止を許す。tdd が壊れたせいでターンが詰まることは無い。

## どう使うか

### 起動（`/tdd` skill）

```
/tdd <実装したい振る舞い> [--cmd "<テストコマンド>"]
  Phase 1  API 設計（スタブのみ: todo!() 等、実装しない）
  Phase 2  テスト記述（まだ実装しない／スタブなので必ず落ちるはず）
  Phase 3  tdd red   --task <id>   →  RED  (落ちること)   → .tdd/<id>.red.json
  Phase 4  実装（テストを GREEN にすることだけが目的）
  Phase 5  tdd green --task <id>   →  GREEN (通ること)    → .tdd/<id>.green.json
  Phase 6  tdd verify --task <id>  →  RED→GREEN を検証
```

タスク ID を 1 つ決め（例: スラッグ `parse-csv`）、以降 `--task <id>` に使う。テストコマンドは `--cmd` 引数を優先し、無ければ `tdd.toml` の `test_cmd`（既定 `cargo test`）。`tdd red` がテストの失敗を確認できなければ「振る舞いをまだ試せていない」ので失敗テストを書き直す。`tdd green` は RED 証跡が無いと拒否する。

`tdd oracle --task <id>` はタスクの証跡から RED→GREEN の遷移を分類し、JSON レポート
（`{"transition":"fail_to_pass","valid_fp_oracle":true,…}`）を出力する。有効な
Fail→Pass のときだけ exit 0（fail-soft: 証跡が無い/壊れている場合は panic せず
`transition:"unknown"` を報告する）。`/tdd` skill 自体のフェーズには含まれず、`condukt` の
Fail→Pass ゲート（`condukt state check-oracle`）が `fix`/`feature` タスクの RED/GREEN 証跡を
決定論的に検証するために呼ぶ機械オラクルである。

### サブコマンド

| サブコマンド | 目的 |
|---|---|
| `tdd gate` | Stop hook 本体。実装が追加されテストが無いとき停止をブロックする。Claude へは常に exit 0 で返し（停止のブロックは exit code ではなく出力 JSON の `decision` フィールドで行う）、手動 CLI でのみ exit 1 を返す。 |
| `tdd red --task <id> [--cmd ...]` | テストを実行し**失敗**を要求して RED 証跡を記録する。既に通っていれば拒否（test-first 不成立）。 |
| `tdd green --task <id> [--cmd ...]` | 先行 RED 証跡を要求し、テストを実行して**成功**を要求し GREEN 証跡を記録する。 |
| `tdd verify --task <id>` | RED と GREEN の両証跡が揃っていれば exit 0。 |
| `tdd oracle --task <id>` | RED→GREEN の遷移を分類し JSON 判定を出力する（上記参照）。有効な Fail→Pass のときだけ exit 0。 |
| `tdd status` | 解決された設定と、cwd に対してゲートが何をするかを表示する。 |
| `tdd init` | スターター `./tdd.toml` を書き出す。 |
| `tdd install` / `tdd uninstall` | `~/.claude/settings.json` に Stop hook をマージ／除去する（プラグインユーザーは不要）。 |
| `tdd trust` | 現在のプロジェクトを信頼登録し、その `tdd.toml` の `test_cmd` を `tdd red`/`green` が honored する（`test_cmd` はそのまま実行されるため、既定では未信頼）。 |

### 設定（`tdd.toml`）

プロジェクト直下の `./tdd.toml`（無ければ `~/.tdd/config.toml`、それも無ければ言語を見た既定値）を読む。主なキー:

```toml
enabled = true
max_attempts = 3            # 連続 N 回ブロックしたら諦めて停止を許す
reset_after_secs = 600      # アイドルがこの秒数を超えると試行カウンタをリセット
min_added_impl_lines = 1    # 追加実装行がこの行数に達したらテストを要求
test_cmd = "cargo test"     # `tdd red` / `tdd green` の既定テストコマンド
proof_dir = ".tdd"          # RED/GREEN 証跡の書き出し先
# impl_globs / test_path_globs / test_markers で言語別の既定を上書き可能
```

### インストール

プラグインマーケットプレイス経由が推奨で、`hooks/hooks.json` を通じて Stop hook を配線し（タイムアウト 30 秒）、`/tdd` skill をバンドルする。プラグインを使わない場合は `tdd install` で Stop hook を `~/.claude/settings.json` にマージし、`tdd init` でスターター `tdd.toml` を書き出す。`/tdd` 使用前に `tdd --version` が通るか確認するとよい（`${CLAUDE_PLUGIN_ROOT}/bin/tdd` が PATH に無ければフルパスで呼ぶ）。

## ライセンス

MIT
