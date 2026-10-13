# difflog

Claude Code 向けの**セッション差分ログ (session diff-log)**。Rust 製。

## 目的

difflog はひとつのセッションで何が変わったかを git 差分として記録し、それを読みやすいナラティブに変換するための harness である。

- `SessionStart` で現在の HEAD の SHA をスナップショットする。
- `SessionEnd` で `git diff <開始SHA>..HEAD` を実行し、構造化された Markdown ログをローカルのログディレクトリへ書き出す。ログにはコミット一覧、stat、変更ファイル一覧、上限付きの diff 本体が含まれる。
- 出力先は `~/.difflog/logs/<YYYY-MM-DD>-<session8>.md`。

差分の記録（決定論的な部分）はひとつの Rust バイナリと 2 つの hook (SessionStart + SessionEnd) だけで完結し、API キーを必要としない。記録されたログから人間向けの説明文を生成する部分は、同梱の `/difflog` skill が LLM を使って担う（サブスクリプションで実行される）。

## どうして必要か

エージェントが行った変更は、セッションが終わるとどこに何を加えたのかが追いにくくなる。レビューや引き継ぎのたびに `git log` や `git diff` を手で追い直すのは手間で、しかもセッション境界（このセッションで何を触ったか）はコミット履歴からは復元しづらい。

difflog はセッション開始時の HEAD を覚えておくことで「このセッションの差分」だけを正確に切り出し、後から読める形で残す。これにより、

- 変更内容を毎回手で diff を組み立てて確認する必要がなくなる。
- レビューや他者への引き継ぎの際に、「何をどう変えたか」「なぜか」を一枚にまとめた説明をすぐ用意できる。

README が引くデータでは、エージェントが diff サマリを添えた場合の開発者による変更の受け入れ率は 89%、生の出力のみの場合は 62% とされている。

## どう使うか

### インストール（プラグイン）

```
/plugin install difflog@yukineko
```

### 手動インストール

```sh
cargo install --path .
difflog install   # ~/.claude/settings.json に hook をマージする
```

`difflog install` が SessionStart / SessionEnd の 2 つの hook を配線する。以降はセッションのたびに自動でスナップショットとログ書き出しが行われる。

### サブコマンド

```sh
difflog session-start   # SessionStart hook（stdin の JSON を読む）
difflog session-end     # SessionEnd hook（ログを書き出す）
difflog last            # 直近のログを表示する
difflog list            # ログファイルを新しい順に一覧する（--limit N、既定 10）
difflog init            # 雛形の ./difflog.toml を書き出す（--force で上書き）
difflog install         # hook を ~/.claude/settings.json にマージする（--dry-run 可）
difflog uninstall       # hook を取り除く（--dry-run 可）
difflog status          # 解決済みの設定を表示する
```

### 設定

`./difflog.toml`（プロジェクト）、無ければ `~/.difflog/config.toml` を読む（先に存在した方が勝ち）。[`difflog.example.toml`](difflog.example.toml) 参照: `enabled`、`log_dir`（既定 `~/.difflog/logs`）、`diff_body_limit`（そのまま含める diff のバイト数。既定 4096、`0` で stat のみ）、`exclude_globs`（組み込み既定: `Cargo.lock`、`package-lock.json`、`*.min.js`、`*.min.css`）。`DIFFLOG_DISABLE` で無効化する。

### `/difflog` でナラティブを生成する

セッション後に slash command を実行すると、ログから LLM がナラティブを生成する。

```
/difflog
```

特定のセッションを対象にする場合は `/difflog --session <id>` を渡す。skill は直近のログ（または指定したセッション）を読み、「変更の概要」「主な変更点」「コミット」「残課題」を含む一枚の人間向けサマリを出力する。ナラティブは生成のみで、ログファイル自体は書き換えない。diff 本体が大きい場合はコスト節約のため stat と name-status だけを読んで概要をまとめる。

決定論的な差分記録は API キー不要で動作し、ナラティブ生成のみサブスクリプション上で実行される。

## ライセンス

MIT
