# session-insights

Claude Code のセッション単位の作業メトリクスを集計するハーネス。Devin の Session Insights に着想を得て、ローカル・API キー不要のフックとして組み直したもの。

## 目的

session-insights は、1 セッションで「実際に何が起きたか」を 2 つのフックで集計する。ツール呼び出し・ターン数・触れたファイルをロールアップし、そこから **サイズクラス（XS–XL）** と **作業カテゴリ**（coding / ops / research / mixed）を導出する。

- **PostToolUse フック**（`record`）が各ツール呼び出しを記録する（ツールごとの回数、触れた個別ファイル）。
- **Stop フック**（`stop`）がターンを 1 つ数え、**SessionEnd フック**（`sessionend`）が任意で Obsidian の日付つきセッションノートを書き出す。

そこから次を導く。

- **size**: 記録したツールイベントの総数による XS / S / M / L / XL（閾値は設定可能）。
- **category**: `coding`（Edit/Write）、`ops`（Bash）、`research`（Read/Grep/Web）、いずれにも偏らなければ `mixed`。
- セッションごとのターン数・ツールイベント数・ファイル数・上位ツール。

集計結果は `session-insights report` で閲覧でき、任意で各セッションを Obsidian vault の日付つきノートとして残せる。サブスクリプションネイティブ——同梱の Rust バイナリ 1 つで動き、デーモンも API キーも要らない。記録は自身の state ディレクトリ（とオプトインで vault）にしか書き込まず、常に exit 0 で終了するため、ターンをブロックすることはない。

## どうして必要か

セッションを何本も回していると、「あのときどれくらいの規模の作業をしたか」「主にコーディングだったのか調査だったのか」が後から分からなくなる。手で記録するのは続かず、かといって LLM の記憶に頼ると曖昧で再現性がない。

- **作業量の客観的な把握がない。** 体感ではなく、ツールイベント数という決定論的な指標から size（XS–XL）を出すことで、セッションの規模を一貫した基準で振り返れる。
- **作業の性質が見えない。** どのツールにどれだけ偏ったかから category（coding / ops / research / mixed）を導くので、セッションが実装中心だったのか調査中心だったのかが一目で分かる。
- **記録の手間とブレ。** フックが自動で数値を埋めるため、人手の集計が不要で、数値は機械所有のブロックとして管理される。`/record` ノートでも散文だけ人間が書き、コスト・トークン・ターンといった数値は session-insights が自動充填する。
- **ターンを止めたくない。** メトリクス収集が失敗してもセッションを妨げてはならない。記録は自分の state（とオプトインの vault）にしか書かず常に exit 0 なので、計測が原因でターンが止まることはない。

なお、セッションをまたぐ未了課題の永続キューは session-insights の責務ではない。それは独立した [`backlog`](../backlog) クレート（`~/.backlog/tasks.toml`）が単一の正典キューとして担い、SessionStart で pending タスクを注入する。session-insights は独自の backlog ストアを持たない。

## どう使うか

### フック配線（プラグイン）

同梱の `hooks/hooks.json` が両フックを自動で配線する。PostToolUse が `record` を、Stop が `stop` を、SessionEnd が `sessionend` を呼ぶ。閾値の調整や Obsidian ログの有効化は `session-insights.toml` を置いて行う。

### レポート

```sh
session-insights report          # 最新セッション
session-insights report --session <id-prefix>
session-insights report --all    # 記録済みセッションを 1 行ずつ
session-insights report --context # context-governor の台帳健全性も併記する
```

```
session a1b2c3d4  [2026-06-20T18:00:00+09:00]
  project: playbook
  size: L   category: coding
  turns: 12   tool events: 47   files: 9
  top tools: Edit 18, Bash 12, Read 9, Write 5, Grep 3
```

### `/record`（Obsidian の記録ノート）

スラッシュコマンド `/record` で、今セッションを Obsidian vault に人間可読の記録ノートとして書く（AEGIS の `/record` 相当）。コスト・トークン・ターン・ファイル・コンテキストといった数値は同梱バイナリが自動充填し、`## 完了サマリ` / `## 認知の変化・修正された理解` / `## つまずき・学び` / `## 振り返り・確立した方針` / `## 自己批判・確信度` / `## 注意点・落とし穴` / `## 残課題` / `## 要追跡・あとで確認` / `## 関連` の散文を人間（実際には Sonnet サブエージェント経由で蒸留）が埋める。

この記録は**成果物の複製ではなく認知の記録**である。backlog / code / commit から読み直せるものは書かず、そこに書けないもの — 推論の経路、修正された理解（初期モデル → どう間違い → 何が直ったか）、確信度の較正 — を書く。ID と`path:line` はポインタに畳む。`## 認知の変化・修正された理解` と `## 自己批判・確信度` の 2 節はそのために置かれており、位置も設計の一部（前者は再解釈する対象の完了サマリの直下、後者は限定する対象の方針の直下）。既存ノートへの遡及追加は行わない（`merge` は見出しの無いノートを書き換えない設計）。

内部では `session-insights record-now` が現在セッション（`$CLAUDE_CODE_SESSION_ID`）を解決し、`## コスト` / `## 数値サマリ` ブロックを更新してノートの絶対パスを出力する。`<!-- si:numeric:* -->` / `<!-- si:cost:* -->` で囲まれたブロックは機械所有のため編集しない。vault ディレクトリが存在しない場合はノートを書かず、その旨を stderr に出して終了する（vault は自動生成しない）。

`/record` のステップ 5 では、独立 `backlog` クレートと突き合わせて、このセッションで片付いた項目を `backlog done <id>` で閉じ、`## 残課題` の未了項目を `backlog add` で登録する。

### backlog（セッション横断の未了課題）

セッションをまたぐ永続キューは、独立した [`backlog`](../backlog) クレート（`~/.backlog/tasks.toml`）が単一の正典として担う。SessionStart で pending タスクを注入するのも backlog 自身のフックだ。session-insights は独自の backlog ストアを持たず、旧 `session-insights backlog` サブコマンドと `<vault>/backlog.md` / `backlog.json` は削除済み。

```sh
backlog add --title "darwin-x86_64 を x86 Mac で再ビルド" --project harness
backlog list --project harness --status pending
backlog done <id>          # 完了項目を閉じる
```

旧 `session-insights backlog` を使っていて state ディレクトリに `backlog.json` が残っている場合は、その open 項目を一度だけ標準 `backlog` に移せばよい（`backlog add` は project+title で重複排除するため冪等）。

```sh
STATE_DIR="$HOME/.session-insights/state"
BACKLOG_JSON="$STATE_DIR/backlog.json"
if [ -s "$BACKLOG_JSON" ]; then
  jq -r '.[] | select(.status=="open") | [.project, .text] | @tsv' "$BACKLOG_JSON" \
    | while IFS=$'\t' read -r project text; do
        [ -n "$text" ] && backlog add --title "$text" --project "${project:-default}"
      done
else
  echo "no backlog.json to migrate (nothing to do)"
fi
```

移行後は `backlog.json`（および session-insights が生成しなくなった `<vault>/backlog.md`）を削除してよい。

### Obsidian ログ（オプトイン）

`obsidian_log = true` にして `obsidian_vault` を自分の vault に向けると、SessionEnd ごとにセッションが `<vault>/sessions/<date>-<id>.md` にフロントマター（`type: session`、size、category、turns…）つきで書き出される——ただし vault ディレクトリが既に存在する場合のみ（vault は決して作らない）。

### スタンドアロン（cargo）

```sh
cargo install --path .
session-insights install      # PostToolUse + Stop + SessionEnd フックをマージ
session-insights report --all
session-insights status        # 解決済み設定を表示
session-insights uninstall
```

`install` / `uninstall` は冪等で、`settings.json` をバックアップし、他プラグインのフックグループを保持する。

### 設定

`./session-insights.toml`（プロジェクト）または `~/.session-insights/config.toml`（グローバル）を置く。最初に見つかったものが優先される。`size_thresholds`（XS–XL の下限）、`ignore_tools`、`obsidian_log` / `obsidian_vault`、`state_dir` などを調整できる。`SESSION_INSIGHTS_DISABLE=1` で無効化できる。

### ビルド

```sh
make bins     # bin/session-insights-darwin-<arch> と -linux-x86_64 を更新
cargo test
```

コミット済みの `bin/session-insights-*` バイナリがプラグインの実体なので、エンドユーザーは cargo も API キーも不要だ。
