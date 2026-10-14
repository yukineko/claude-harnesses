# budgetguard

> 🌐 [English](README.md) ・ **日本語**

**Claude Code 向けリアルタイム・コスト予算ゲート (Rust 製)**

## 目的

budgetguard は、Claude Code セッションの **見積もりコスト (USD)** を監視し、設定した上限を
超えたら Stop を **ブロックする** ハーネスである。gauge が「観測する」のに対し、budgetguard は
「制御する」。

`budgetguard gate` は Claude Code の **Stop** フックに配線される。各 Stop で次を行う:

1. このセッションのモデルごとのトークン使用量を得る。gauge が永続化したセッションレコードが
   トランスクリプト以降に更新されていればそれを使い、そうでなければトランスクリプト (JSONL) を
   読み直して集計する (sub-agent の支出も含む)。
2. 組み込みの料金テーブルで USD コストを見積もる (gauge と同じ料金表)。
3. ローカルの **日次台帳** (`~/.budgetguard/state/ledger.json`) を、このセッションの最新コストで
   更新する。並行セッションとは台帳ロックで直列化する (日付キーは**ローカル**の 0 時で切り替わる)。
4. セッション合計とその日の累積合計を、設定した上限と照合する。
5. 判定を出す:
   - **上限内** → exit 0、Stop はそのまま進む。
   - **warn 閾値超過** → `{"additionalContext":"…"}` を出す (助言のみ、ブロックしない)。
   - **block 閾値超過** → `{"decision":"block","reason":"…"}` を出し、ターンが終わる前に
     エージェントへ保存・コミットを促す。
   - 閾値は「上限以上」で判定し、block が warn に優先する。

あわせて gauge のセッションレコードから **キャッシュヒットの健全性** を毎 Stop で stderr に報告する。
`input + cache_read` が `[cache].min_tokens` (既定 200000) に達した後、ヒット率が `[cache].min_rate`
(既定 0.5。範囲外の値は既定値に戻る) を下回ると、より強い通知が無い場合に限り助言の
`additionalContext` に昇格する。ブロックはしない。

ブロックした Stop は overwatch の violation event も記録する。

超過通知はエージェントに差し戻されるので、エージェントは作業を保存して優雅に切り上げられる。

これは検証ゲート群のコスト制御版の兄弟である:

| ゲート | タイミング | 問い |
|---|---|---|
| `donegate` | Stop 時 | ビルドが通り、テストが通るか? |
| `reviewgate` | Stop 時 | コード品質は許容できるか? |
| **`budgetguard`** | **Stop 時** | **コストは予算内か?** |

API キーは不要。トランスクリプトは既にディスク上にあり、budgetguard はそれを決定論的に読む。
何もマシンの外には出ない。

## どうして必要か

LLM エージェントを回しっぱなしにすると、コストは静かに積み上がる。gauge のような
観測ツールは「いくら使ったか」を後から教えてくれるが、それは事後報告であり、走っている
ターンを止めはしない。暴走したループや想定外に高価なセッションは、誰かがダッシュボードを
見るまで膨らみ続ける。

budgetguard はこの空隙を埋める。コストを Stop ゲートに変えることで、セッション単位・日次の
**ハードな上限**を強制し、超過分をエージェント自身に差し戻して安全に着地させる。観測だけでなく
**制御**が要る場面のためのハーネスである。

許可されるもの (exit 0、判定なし):

- 設定ファイルが無い、または `[session]` / `[daily]` の上限がすべて `0` なら、すべての Stop を許可する
  (上限 0 は「上限なし」)。
- 値付けすべきトランスクリプトのデータが無い。
- `stop_hook_active` 付きの再入 (前の Stop フックが既にブロックした) — 予算超過のセッションは
  一度警告された後、停止を許される。
- `BUDGETGUARD_DISABLE=1` ならフックは no-op になる (何よりも先に判定する)。

*判定不能* は予算内として**扱わない**:

- 存在するが読めない／parse できない設定ファイル → 上限が何であれ**ブロック** (`config-undetermined`)。
- セッション支出を測定できない (sub-agent のトランスクリプトが読めない等) → block 上限が 1 つでも
  あれば**ブロック**、無く warn 上限があれば**警告**。
- 日次合計を測定できない (台帳ロックを他セッションが保持、または `ledger.json` が破損 — 破損ファイルは
  そのまま保存される) → *日次*上限に従って block/warn。セッション自身の block 上限を既に超えていれば
  その具体的な理由を返す。
- ゲート内の panic は **fail closed** (`harness_core::gate::run::run_guarded` がブロックしクラッシュを
  表面化させる)。`stop_hook_active` で有界。
- 測定できなかった合計は stderr に `$0` ではなく `unknown` と表示される。

## どう使うか

### 導入 (プラグイン)

マーケットプレイス経由 (カタログは本リポジトリ `yukineko/claude-harnesses` のルートにある):

```
/plugin marketplace add yukineko/claude-harnesses
/plugin install budgetguard@yukineko
```

**サブスクリプション完結** — Stop フック 1 本と同梱の Rust バイナリだけで動き、`ANTHROPIC_API_KEY`
も追加インストールも要らない。

### 手動導入 (ソースから)

```sh
cargo install --path .
budgetguard init          # 雛形の ./budgetguard.toml を書き出す
budgetguard install       # Stop フックを ~/.claude/settings.json にマージする
```

### 設定

プロジェクトルートに `budgetguard.toml` を置くか、グローバル既定として
`~/.budgetguard/config.toml` を置く。全オプションは `budgetguard.example.toml` を参照。

```toml
[session]
warn_usd  = 0.50
block_usd = 2.00

[daily]
warn_usd  = 5.00
block_usd = 20.00
```

プロジェクトのファイルがあれば home のファイルの*代わりに*使われる (マージはしない)。trust ゲートは
無く、リポジトリ同梱の `budgetguard.toml` もそのまま honor される。他のキー: `enabled`、`state_dir`
(既定 `~/.budgetguard/state`)、`[[price]]`、`[cache]` (`min_rate`, `min_tokens`)。

### サブコマンド

```sh
budgetguard gate           # Stop フック (stdin の JSON を読み、判定を出す)
budgetguard status         # 解決済みの設定 + 今日の支出
budgetguard status --json  # 機械可読な予算プレッシャー (fugu-router 向け):
                           #   {day_usd, daily_warn_usd, daily_block_usd, pressure}。
                           #   設定が読めなければ {error, undetermined:true} を出し exit 2
budgetguard init [--force]          # 雛形の budgetguard.toml を書き出す
budgetguard install [--dry-run]     # フックを ~/.claude/settings.json にマージする
budgetguard uninstall [--dry-run]   # フックを取り除く
```

### 料金

`harness-core` の組み込み料金テーブルを使う (gauge と同じ):

| ファミリ | 入力 $/1M | 出力 $/1M |
|---|---|---|
| Fable / Mythos | 10 | 50 |
| Opus | 5 | 25 |
| Sonnet | 3 | 15 |
| Haiku | 1 | 5 |

`budgetguard.toml` の `[[price]]` スタンザで任意のモデルの料金を上書きできる（`pattern` はモデル id に対する大文字小文字を区別しない部分一致）。表にも上書きにも無いモデルの使用量の費用は 0 ではなく **不明** であり、ゲートはそのセッションを未計測（`cost-undetermined`）として扱う。本当に無料のモデルは `input = 0.0` / `output = 0.0` で宣言する。`pattern` が空（全モデルに一致してしまう）または単価が有限の非負数でない `[[price]]` 行は、パース失敗と同じく設定全体を使用不能にする。

## ライセンス

MIT
