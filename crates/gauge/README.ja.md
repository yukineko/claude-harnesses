記録はセッションごとに 1 レコード（`<store>/sessions/<session_id>.json`）として保存される。
レコードは毎ターン上書きされ、最新の書き込みがここまでのセッション全体を保持する：

- モデル別の input / output / **キャッシュ書き込み (5分・1時間 TTL)** / **キャッシュ読み込み** トークン
- モデルリクエスト数（ターン数）とツール別の呼び出し回数（Bash, Edit, …）
- 最初/最後のタイムスタンプ → セッション継続時間

**推定コスト** はレコードの読み出し時に組み込み価格表（モデル単位で上書き可能）から算出される。

`gauge report` が、プロジェクト・モデル・日付ごとにこれらを集計する。
`gauge subagents` は、サブエージェント（Task）ごとのトランスクリプトからコストを個別に按分する。

記録を完全に止めたいときは `GAUGE_DISABLE=1` を指定する。

Claude Code プラグインとしては、バンドル済みの `bin/gauge` が `hooks/hooks.json` 経由（`${CLAUDE_PLUGIN_ROOT}/bin/gauge record`）で Stop フックとして自動的に呼ばれる。
追加設定なしで記録が始まる。

スタンドアロンでは次を実行せよ。

```sh
cargo install --path .
gauge install        # Stop フックを ~/.claude/settings.json にマージする
gauge uninstall      # 元に戻す
```

```sh
gauge report                       # 合計とプロジェクト/モデル/日別の内訳
gauge report --project myrepo      # プロジェクトで絞り込む
gauge report --since 2026-06-01    # 日付で絞り込む
gauge session                      # 直近セッションの詳細
gauge session --session <ID> --json  # 指定セッションを {session_id, cost_usd, models, agents} の JSON で
gauge subagents                    # サブエージェント(Task)別のコスト按分
gauge subagents --session <ID> --json  # JSON 配列。時刻フィールドは不明なら null
gauge status                       # 解決済み設定・ストアパス・セッション数
gauge init [--force]               # スターター ./gauge.toml を書き出す(--force で上書き)
```

`gauge install` / `gauge uninstall` は `--dry-run` を受け付ける。

```sh
gauge config show                  # 登録済みの窓設定があれば表示する
```

組み込みレート（USD / 100万トークン, input/output）: **Opus** 5/25 ・ **Sonnet** 3/15 ・ **Haiku** 1/5 ・ **Fable/Mythos** 10/50。
キャッシュ書き込みは input の 1.25倍（5分 TTL）または 2倍（1時間 TTL）、キャッシュ読み込みは input の 0.1倍で課金される。
認識できないモデルの使用量の費用は 0 ではなく **不明** として扱われ、`unknown` と表示される。
JSON では `cost_usd: null` と `cost_undetermined` になる。
budgetguard はそのセッションを未計測とみなす。
本当に無料のモデルは `input = 0.0` / `output = 0.0` の明示的な上書きで宣言する。
`gauge.toml` でモデルを上書き・追加できる。

```toml
[[pricing]]
pattern = "opus"   # モデル id への部分一致。最初に一致したものが勝つ
input = 5.0
output = 25.0
```

コストはレポートのたびに再計算されるため、価格表を編集すれば過去分も再評価される。

プロジェクトの `./gauge.toml` > `~/.gauge/config.toml` > 組み込みデフォルトの順（最初に存在したファイルが勝つ）。
`gauge.example.toml` を参照。
ストアの既定値は `~/.gauge/store`。

プロジェクトの `./gauge.toml` が存在するのに読めない・パースできない（`[[pricng]]` や `enabeld` のような typo を含む未知キーはすべてパース失敗扱い）、または `[[pricing]]` 行に `pattern`/`input`/`output` が欠けている・単価が有限の非負数でない場合、既定値へ黙って落ちない。読み取りコマンド（`report` / `session` / `subagents` / `status`）はファイル名を含むメッセージを出して exit 1 し、`record` hook は何も記録せず stderr に理由を出す。

```sh
cargo build --release
cargo test
```
