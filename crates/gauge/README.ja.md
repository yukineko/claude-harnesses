# gauge

Claude Code 向けのローカル LLMOps テレメトリ。Stop フックを兼ねた単一の Rust バイナリが、毎ターンごとにセッションの記録を集計し、ローカルに保存・集計表示する。

## 目的

gauge は、自分のエージェント実行（Claude Code セッション）の使用量を計測・記録する観測層である。

Stop フックとして動作し、ターンが終わるたびにセッションのトランスクリプト（JSONL）を読み直して、累積の **トークン使用量・キャッシュヒット・ツール呼び出し・レイテンシ・推定コスト** をローカルストアに記録する。記録はセッションごとに 1 レコード（`<store>/sessions/<session_id>.json`）として、毎ターン上書きされる。各レコードには次が含まれる。

- モデル別の input / output / **キャッシュ書き込み (5分・1時間 TTL)** / **キャッシュ読み込み** トークン
- モデルリクエスト数（ターン数）とツール別の呼び出し回数（Bash, Edit, …）
- 最初/最後のタイムスタンプ → セッション継続時間
- 組み込み価格表からの **推定コスト**（モデル単位で上書き可能）

その後 `gauge report` が、プロジェクト・モデル・日付ごとにこれらを集計する。さらに `gauge subagents` は、サブエージェント（Task）ごとのトランスクリプトからコストを個別に按分するため、condukt のような呼び出し元はセッション合計の一括値ではなく、タスク単位の実コストを記録できる。

ツールキットの他のハーネスと同じく **subscription-native**（フック 1 本とバンドル済みバイナリのみ、**API キー不要**、**マシンの外にデータは出ない**）で完結する。

## どうして必要か

エージェント実行のコストやトークン消費は、可視化しないと「気づかないうちに膨らむ」典型的な失敗モードに陥る。どのプロジェクト・どのモデル・どの日にいくら使ったのか、キャッシュが効いているのか、どのツールを何回呼んでいるのか——これらは後から手作業で追えない。

gauge はこれを自動の Stop フックで賄うため、計測のために手順を増やす必要がない。フックは **観測のみ** を行い、panic-guard 付きで常に exit 0 を返す。そのため、不正な stdin・トランスクリプトの欠落・ストアへの書き込み失敗があっても、その分を記録しないだけでターンを壊さない。記録を完全に止めたいときは `GAUGE_DISABLE=1` を指定する。

コストはレポートのたびに保存済みトークン数から再計算されるため、価格表を編集すれば過去分も再評価される。また `gauge subagents` によってサブエージェント単位の実コストが取れるので、オーケストレーター側が一括合計でしか見られない問題も解消する。

## どう使うか

Claude Code プラグインとしては、バンドル済みの `bin/gauge` が `hooks/hooks.json` 経由（`${CLAUDE_PLUGIN_ROOT}/bin/gauge record`）で Stop フックとして自動的に呼ばれる。追加設定なしで記録が始まる。

スタンドアロンで使う場合:

```sh
cargo install --path .
gauge install        # Stop フックを ~/.claude/settings.json にマージする
gauge uninstall      # 元に戻す
```

集計・確認の主なサブコマンド:

```sh
gauge report                       # 合計とプロジェクト/モデル/日別の内訳
gauge report --project myrepo      # プロジェクトで絞り込む
gauge report --since 2026-06-01    # 日付で絞り込む
gauge session                      # 直近セッションの詳細
gauge subagents                    # サブエージェント(Task)別のコスト按分
gauge status                       # 解決済み設定・ストアパス・セッション数
gauge init                         # スターター ./gauge.toml を書き出す
```

`gauge report` の出力例:

```
gauge — 2 セッション / 310 turns
合計コスト $54.08  ·  トークン 49.32M (49,321,292)

プロジェクト別
  myrepo                      $54.08    49.32M  2 sess

モデル別
  claude-opus-4-8             $54.08  in 47.5k / out 493.0k / cache 48.78M

日別 (直近14日)
  2026-06-20     $54.08    49.32M
```

### レートリミット窓（手動登録・任意）

アカウントのレートリミット窓の残り時間を取得する API は存在しないため自動検出はできない。自分の `/usage` 画面を見て手動で登録する:

```sh
gauge config set-window --hours 5 --last-reset 2026-07-16T09:00:00Z
gauge config show                  # 現在登録されている窓設定を表示（未設定なら何も出さない）
```

未設定の場合 `gauge status`/`gauge report` は連続稼働時間のみを表示する（これは正常・想定内の状態でありエラーではない）。`condukt shadow-run`（同様に自動発火の仕組みを持たない）をいつ有効化するかを判断する手動入力として使える。

### 価格

組み込みレート（USD / 100万トークン, input/output）: **Opus** 5/25 ・ **Sonnet** 3/15 ・ **Haiku** 1/5 ・ **Fable/Mythos** 10/50。キャッシュ書き込みは input の 1.25倍（5分 TTL）または 2倍（1時間 TTL）、キャッシュ読み込みは input の 0.1倍で課金される。認識できないモデルの寄与は 0。`gauge.toml` でモデルを上書き・追加できる。

```toml
[[pricing]]
pattern = "opus"   # モデル id への部分一致。最初に一致したものが勝つ
input = 5.0
output = 25.0
```

### 設定

プロジェクトの `./gauge.toml` > `~/.gauge/config.toml` > 組み込みデフォルトの順（最初に存在したファイルが勝つ）。`gauge.example.toml` を参照。ストアの既定値は `~/.gauge/store`。

## ビルド

```sh
cargo build --release        # バイナリは target/release/gauge
cargo test
make bins                    # バンドル済み bin/gauge-darwin-* と -linux-x86_64 を更新
```

Linux 版は cargo-zigbuild で macOS からクロスコンパイルされ（Docker 不要）、古い glibc フロアに固定して各ディストロで動くようにしている。
