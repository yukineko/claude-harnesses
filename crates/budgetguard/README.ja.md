`budgetguard gate` は Claude Code の **Stop** フックに配線され、各 Stop で次を行う:

1. このセッションのモデルごとのトークン使用量を得る。
   gauge が永続化したセッションレコードが
   トランスクリプト以降に更新されていればそれを使い、そうでなければトランスクリプト (JSONL) を
   読み直して集計する (sub-agent の支出も含む)。
2. 組み込みの料金テーブルで USD コストを見積もる。
3. ローカルの **日次台帳** (`~/.budgetguard/state/ledger.json`) を、このセッションの最新コストで
   更新する (Stop ごとの合計ではない)。
   書き込みは台帳ロックの下で行い、他セッションがロックを保持している間は台帳を書かず、日次合計を未計測とする。
   日付キーは**ローカル**の日付である。
4. セッション合計とその日の累積合計を、設定した上限と照合する。
5. 判定を出す:
   - **上限内** → exit 0、Stop はそのまま進む。
   - **warn 閾値超過** → `{"additionalContext":"…"}` を出す (助言のみ、ブロックしない)。
   - **block 閾値超過** → `{"decision":"block","reason":"…"}` を出し、ターンが終わる前に
     エージェントへ保存・コミットを促す。
   - 閾値は「上限以上」で判定し、block が warn に優先する。

あわせて gauge のセッションレコードから **キャッシュヒットの健全性** を stderr に報告する。
`input + cache_read` が `[cache].min_tokens` (既定 200000) に達した後、ヒット率が `[cache].min_rate`
(既定 0.5、範囲外の値は既定値に戻る) を下回ると、より強い通知が無い場合に限り助言の
`additionalContext` に昇格する。
ブロックはしない。

ブロックした Stop は overwatch の violation event も記録する。

### 上限が設定されていないとき

上限 `0` はその軸だけを無効にする。上限が**1つも**無い場合
(設定ファイルが無い、または `[session]` / `[daily]` の warn/block 上限がすべて `0`) は「上限なし」ではなく、budgetguard が上限を決める:

- **derived (経験値)**: 台帳に過去 7 日以上 (今日を除く) あれば、その履歴から外れ値を Tukey 法で除いて決める
  (Q1/Q3 は線形補間、fence = Q3 + 1.5 × IQR)。日次 block = 日次合計の fence、日次 warn = fence 以下の日の p90。
- **default (既定値)**: 履歴が足りなければ組み込みの値 — 日次 warn $1336 / block $1871
  (2026-10-19 にメンテナの台帳から同じ方法で算出)。
- **セッション軸**には warn も block も自動設定しない。セッションの fence (過去の各セッション記録に対する Tukey fence、既定値 $306) は
  `check` / `status --json` の `session_fence_usd` として情報表示だけされる。セッションの warn/block は設定で明示したときだけ効く。
- 存在するが読めない／parse できない台帳、または有限でない・負のコストを含む台帳は**判定不能**: gate はブロックし、
  `check`/`status` は exit 2。既定値で黙って置き換えない。

`check` と `status --json` は上限の出どころを `limits_source` (`config` | `derived` | `default`) で報告する。

許可されるもの (exit 0、判定なし):

- `stop_hook_active` 付きの再入 (前の Stop フックが既にブロックした) — 予算超過のセッションは
  一度ブロックされた後、停止を許される。
- `BUDGETGUARD_DISABLE=1` ならフックは no-op になる (設定ファイルが parse できない場合も同じ)。
- 設定で `enabled = false` なら、parse できる Stop payload に対してフックは no-op になる (parse できない stdin はブロック)。

*判定不能* は予算内として**扱わない**:

- 存在するが読めない／parse できない設定ファイル → 上限が何であれ**ブロック** (`config-undetermined`)。
- セッション支出を測定できない (sub-agent のトランスクリプトが読めない、料金不明のモデルがある等) → block 上限が 1 つでも
  あれば**ブロック**、無く warn 上限があれば**警告**。
  存在しない／ディレクトリである `transcript_path`、`session_id` か `transcript_path` が空の Stop payload もこれに含まれる。
- stdin がそもそも hook payload でない場合、上限の有無に関わらず常に**ブロック** (stdout に `{"decision":"block"}`、exit 0)。入力を処理できないためであり、exit 1 は Claude Code では stop を通す non-blocking error になる。`enabled = false` でもブロックする。ブロックには上限があり、連続 3 回までブロックし、4 回目は gate が諦めた旨の `systemMessage` を出して通す。連続回数は `CLAUDE_CODE_SESSION_ID` ごと (無ければ 1 つの共通キー) に `<state_dir>/unparseable-streak/` へ記録し、parse できた Stop でリセットする。記録を読み書きできないときは明示的なメッセージ付きで通す。`BUDGETGUARD_DISABLE=1` なら完全に迂回する。
- 日次合計を測定できない (台帳ロックを他セッションが保持、または `ledger.json` が破損 — 破損ファイルは
  そのまま保存される) → *日次*上限に従って block/warn。
  セッション自身の block 上限を既に超えていれば
  その具体的な理由を返す。
- 測定できなかった合計は stderr に `$0` ではなく `unknown` と表示される。

本リポジトリのルートにあるマーケットプレイスのカタログに budgetguard が載っている:

```
/plugin marketplace add yukineko/claude-harnesses
/plugin install budgetguard@yukineko
```

```sh
cargo install --path .
budgetguard init          # 雛形の ./budgetguard.toml を書き出す
budgetguard install       # Stop フックを ~/.claude/settings.json にマージする
```

プロジェクトルートに `budgetguard.toml` を置くか、グローバル既定として
`~/.budgetguard/config.toml` を置く。
`budgetguard.example.toml` はそのまま使える雛形で、`enabled`、コメントアウトされた `state_dir`、`[session]`、`[daily]`、コメントアウトされた `[[price]]` を含む。

```toml
[session]
warn_usd  = 0.50
block_usd = 2.00

[daily]
warn_usd  = 5.00
block_usd = 20.00
```

プロジェクトのファイルがあれば home のファイルの*代わりに*使われる (マージはしない)。
trust ゲートは
無く、リポジトリ同梱の `budgetguard.toml` もそのまま honor される。
他のキー: `enabled`、`state_dir`
(既定 `~/.budgetguard/state`)、`[[price]]`、`[cache]` (`min_rate`, `min_tokens`)。

```sh
budgetguard gate           # Stop フック (stdin の JSON を読み、判定を出す)
budgetguard status         # 解決済みの設定 + 今日の支出
budgetguard status --json  # 機械可読な予算プレッシャー:
                           #   {day_usd, daily_warn_usd, daily_block_usd, pressure}。
                           #   設定が読めなければ {error, undetermined:true} を出し exit 2。
                           #   ledger.json が破損していても同じ
budgetguard init [--force]          # 雛形の budgetguard.toml を書き出す。--force が無ければ既存ファイルは変更しない
budgetguard install [--dry-run]     # フックを ~/.claude/settings.json にマージする
budgetguard uninstall [--dry-run]   # フックを取り除く
```

`harness-core` の組み込み料金テーブルを使う:

| ファミリ | 入力 $/1M | 出力 $/1M |
|---|---|---|
| Fable / Mythos | 10 | 50 |
| Opus | 5 | 25 |
| Sonnet | 3 | 15 |
| Haiku | 1 | 5 |

`budgetguard.toml` の `[[price]]` スタンザで任意のモデルの料金を上書きできる（`pattern` はモデル id に対する大文字小文字を区別しない部分一致）。表にも上書きにも無いモデルの使用量の費用は 0 ではなく **不明** であり、ゲートはそのセッションを未計測（`cost-undetermined`）として扱う。本当に無料のモデルは `input = 0.0` / `output = 0.0` で宣言する。`pattern` が空（全モデルに一致してしまう）または単価が有限の非負数でない `[[price]]` 行は、パース失敗と同じく設定全体を使用不能にする。

ライセンス: MIT。
