メトリクス収集が失敗してもセッションを妨げてはならない。

backlog / code / commit から読み直せるものは書かないこと。

`<!-- si:numeric:* -->` / `<!-- si:cost:* -->` で囲まれたブロックは機械所有のため編集しない。

`merge` は機械所有マーカーの無い既存ノートを書き換えない（既存ノートへ遡及追加しない）。

 Devin の Session Insights に着想を得て、ローカル・API キー不要のフックとして組み直したもの。

- **PostToolUse フック**（`record`）が各ツール呼び出しを記録する（ツールごとの回数、触れた個別ファイル）。
- **Stop フック**（`stop`）がターンを 1 つ数える。`obsidian_log = true` なら Obsidian の日付つきセッションノート（`sessions/`）も毎ターン書き直し、`record = true` なら `/record` ノートの数値ブロックも更新する。
- **SessionEnd フック**（`sessionend`）は `record = true` のときだけ `/record` ノートを書き出す／更新する。
- フックはブロックせず exit 0 だが、空・パース不能なペイロードを黙って捨てない。各フックはそのイベントを記録しなかったことを stderr に出す。`stop` は加えて `<state_dir>/log.jsonl` に `{"event":"unparseable_payload","hook":"stop",...}` の行を追記し、1行だけの `systemMessage`「`session-insights: セッションを記録できませんでした`」を出す（2026-10-27 のユーザー裁定「Stop時に結果の報告はあってもいいがログはいらない」。結果は1行、詳細はログへ）。

- **size**: 記録したツールイベントの総数による XS / S / M / L / XL（閾値は設定可能）。
- **category**: `coding`（Edit/Write/MultiEdit/NotebookEdit）、`ops`（Bash）、`research`（Read/Grep/Glob/WebFetch/WebSearch）のうち、分類対象イベントの半分以上を占めるグループ。どれも半分に届かなければ `mixed`、分類対象が 0 件なら `empty`。
- セッションごとのターン数・ツールイベント数・ファイル数・上位ツール。

同梱の `hooks/hooks.json` が 3 つのフックを自動で配線する（PostToolUse の matcher は `Bash|Edit|MultiEdit|Write|Read|Grep|Glob|WebFetch|WebSearch|NotebookEdit`）。PostToolUse が `record` を、Stop が `stop` を、SessionEnd が `sessionend` を呼ぶ。閾値の調整や Obsidian ログの有効化は `session-insights.toml` を置いて行う。

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

セッションストアが読めない場合、`report` は「セッション無し」とは表示せず、stderr に `unknown — session store could not be read: …` を出して exit 1 で終わる。個々のセッションファイルが読めない・パースできない場合も黙って飛ばさない。`report --all` は読めたセッションを一覧したうえで、読めなかったファイルを stderr に `incomplete — ` に続けて列挙し exit 1 する。`report`（最新）と `report --session` は、読めないファイルが答えでありうるとき `unknown` として exit 1 する。

記録ノートの数値ブロック（`## 数値サマリ` / `## コスト`）は同梱バイナリが充填し、`## 完了サマリ` / `## 認知の変化 / 修正された理解` / `## つまずき / 学び` / `## 振り返り / 確立した方針` / `## 自己批判 / 確信度` / `## 注意点 / 落とし穴` / `## 残課題` / `## 要追跡 / あとで確認` / `## 関連` の散文節は `<!-- fill: … -->` プレースホルダのまま残る。

backlog / code / commit に書けないもの — 推論の経路、修正された理解（初期モデル → どう間違い → 何が直ったか）、確信度の較正 — を書くこと。ID と `path:line` はポインタに畳むこと。`## 認知の変化 / 修正された理解` は `## 完了サマリ` の直下、`## 自己批判 / 確信度` は `## 振り返り / 確立した方針` の直下に置かれる。

`record = true` のときは Stop（毎ターン）と SessionEnd でもノートの数値ブロックが自動更新される。`record-now` は `record = false` でも書き込む。内部では `session-insights record-now` が現在セッション（`$CLAUDE_CODE_SESSION_ID`）を解決し、`## コスト` / `## 数値サマリ` ブロックを更新してノートの絶対パスを出力する。vault ディレクトリが存在しない場合はノートを書かず、その旨を stderr に出して終了する（vault は自動生成しない）。ノートを書かなかったときの終了コードは 1 である（stdout は空）。

セッションをまたぐキューは、独立した [`backlog`](../backlog) クレート（リポジトリ単位の `<repo root>/.backlog/tasks.toml`）が担う。pending タスクを注入するのは `backlog session-start` コマンドだ。session-insights は独自の backlog ストアを持たず、旧 `session-insights backlog` サブコマンドと `<vault>/backlog.md` / `backlog.json` は削除済み。

```sh
backlog add --title "darwin-x86_64 を x86 Mac で再ビルド" --project harness --repro-test "<cmd>"
backlog list --project harness --status pending
backlog done <id> --test "cargo test -p x" --red-rev <rev>   # 証跡つきで閉じる
```

ストアへの書き込み（`add` / `done` など）は primary の作業ツリーでは拒否される。
linked worktree から実行すること。
素の `backlog done <id>` は拒否される。
閉じるには証跡（`--test … --red-rev …`、`--doc-only <commit>`、`--duplicate-of <id>` のいずれか）が要る。
再現する `--repro-test` の無い `add` は拒否され、store に何も書かない。
詳細は backlog の README を参照すること。

`obsidian_log = true` にして `obsidian_vault` を自分の vault に向けると、Stop（毎ターン）ごとにセッションが `<vault>/sessions/<date>-<id>.md` にフロントマター（`type: session`、size、category、turns…）つきで書き出される——ただし vault ディレクトリが既に存在する場合のみ（vault は決して作らない）。

```sh
cargo install --path .
session-insights install      # PostToolUse + Stop + SessionEnd フックをマージ
session-insights report --all
session-insights status        # 解決済み設定を表示
session-insights init          # state ディレクトリを作成
session-insights uninstall
```

`install` / `uninstall` は冪等で、`settings.json` をバックアップし、他プラグインのフックグループを保持する。

`./session-insights.toml`（プロジェクト）または `~/.session-insights/config.toml`（グローバル）を置く。最初に見つかったものが優先される。選ばれたファイルが読めない・パースできない、または `size_thresholds` が 4 要素でない場合は黙って既定値に置き換えず、読み込みのたびに stderr へ出し、`status` は config を INVALID と表示して exit 1 する。

- `enabled` — フック全体の有効/無効; 既定値 `true`
- `size_thresholds` — `[S, M, L, XL]` のツールイベント数の下限; 既定値 `[5, 15, 40, 100]`
- `ignore_tools` — メトリクスから除外するツール; 既定値 `["TodoWrite"]`
- `obsidian_log` — 毎 Stop で日付つき `sessions/` ノートを書く; 既定値 `false`
- `obsidian_vault` — vault ルート（`sessions/` と `records/` の下に書く）; 既定値 `~/Documents/vault/yukineko`
- `record` — Stop と SessionEnd で `/record` ノートを書く/更新する; 既定値 `false`
- `record_dir` — record ノートを置く vault 内サブディレクトリ; 既定値 `records`
- `state_dir` — ロールアップ state ディレクトリ; 既定値 `~/.session-insights/state`
- `price_overrides` — `## コスト` ブロックのモデル別単価上書き; 既定値 （組み込み単価）

`SESSION_INSIGHTS_DISABLE` に `0` 以外の空でない値を設定すると無効化できる。

```sh
make bins
cargo test
```

コミットされているのは `bin/session-insights` ランチャーだけで、プラットフォーム別の `bin/session-insights-<os>-<arch>` は git 管理外（.gitignore）。ホスト用ビルドが無い場合、ランチャーは「実行した」ふりをしない: `record` は stderr に 1 行、`stop` は `systemMessage` を出し、`sessionend` は exit 1、CLI 動詞は exit 127 で終わる。
