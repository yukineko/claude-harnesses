**read-only** であり、HOME・作業ディレクトリ・TMPDIR の下に何も書き込まず、他プラグインのストアを集約するだけである。

汎用の `SessionStart` ダッシュボード hook は登録**しない**。

**Claude Code 向けの統合 HOTL ステータスダッシュボード**。

- **予算 (budgetguard)**: 今日の支出額とセッション数。`~/.budgetguard/state/ledger.json` から読む。
- **直近セッション (gauge)**: 直近 N 件のセッションの turn 数・トークン数・USD コスト。`~/.gauge/store/sessions/` から読む。
- **進捗ファイル**: カレントディレクトリの `.claude/progress.md` のプレビュー。
- **Stop-hook レイテンシ**: `~/.harness/state/hook-latency.jsonl` からのセッション単位の合計。
- **UserPromptSubmit 注入サイズ**: `~/.harness/state/inject-metrics.jsonl` からのターン単位の合計。
- **hook binary の健全性**: `~/.claude/settings.json` に登録された hook のうち、binary がディスク上に無いもの。
- **PATH shadowing**: plugin cache の同名 binary を PATH 上で覆い隠している PATH 上のコピー。

`SessionStart` hook (`hooks/hooks.json`) を 1 本だけ登録し、hook-binary health check (`harness-status session-start`) を実行する。
`~/.claude/settings.json` に登録された hook の binary がディスク上に無い場合、`additionalContext` を注入する。
plugin cache の binary が PATH 上のコピーに shadow されている場合、`additionalContext` を注入する。
PATH-shadow スキャン自体が完了しなかった場合 (判定不能として報告)、`additionalContext` を注入する。
それ以外のときは無出力・無注入でサイレントである。

```
╔══════════════════════════════════════════════╗
║         harness-status  (2026-06-23)         ║
╚══════════════════════════════════════════════╝

── Budget (budgetguard) ──────────────────────────
  Today spend:  $1.8420  (3 session(s))

── Recent sessions (gauge) ───────────────────────
  Session          Project              Turns       Tokens  Cost USD
  ----------------------------------------------------------------------
  3c8d91a2         harness                 12        35000    0.1850

── Progress file ─────────────────────────────────
  cwd: /repo
  /repo/.claude/progress.md
  │ - specforge ⑤ worktree merge
```

```
/plugin install harness-status@yukineko
```

導入後、任意のセッションで `/status` を実行する。

```sh
cargo install --path .
harness-status
```

```sh
harness-status                       # フルダッシュボード
harness-status budget                # 今日の支出のみ
harness-status sessions --sessions 10  # 直近セッション (件数 N を指定)
harness-status progress              # 進捗ファイルのみ
harness-status hooks                 # Stop-hook レイテンシ集約 (予算モニタ)
harness-status inject                # UserPromptSubmit 注入サイズ集約 (予算モニタ)
harness-status plugins               # 全プラグインを活性化スコープで分類
harness-status hooks-health          # settings.json の hook のうち binary 欠落のもの
harness-status path-shadow           # plugin cache 版を覆う PATH 上の binary
harness-status --json                # 機械可読出力 (フルダッシュボード・budget・sessions・progress・hooks・inject・plugins・hooks-health・path-shadow で有効)
```

`plugins` サブコマンドは全プラグインを **always-on** / **event-scoped** / **manual** に分類する。
チェックアウトから実行すること。リポジトリのレイアウトから分類する。

- ゼロ・空のパネル・沈黙した hook health check を、問題なしの証明として読まないこと。
- プラグインのストアが存在しないとき、そのセクションは "not installed?" と表示し、コマンドは exit 0 で終わる。
- `sessions` と `path-shadow` は読み取り元を読めないとき exit 1 で終わる: gauge のストアが読めない場合 `sessions` は stderr に `sessions: unknown — …` を出し (`--json` では stdout に `{"status":"unknown",…}`)、PATH スキャンが完了しない場合 `path-shadow` は stderr に `harness-status: PATH-shadow scan could not be completed: …` を出す。フルダッシュボードはどちらも `unknown — …` と表示する。
- `harness-status hooks` は、Stop-hook の合計実行時間が `HARNESS_HOOK_LATENCY_BUDGET_MS` (デフォルト `30000`、数値として読めない値はデフォルトに戻る) を超えるセッションを警告する。
- `harness-status inject` は、UserPromptSubmit 注入の合計が `HARNESS_INJECT_BUDGET_CHARS` (デフォルト `20000`) を超えるターンを警告する。
- 日付はシステムクロックから導出する。
- テスト時は `HARNESS_DATE=YYYY-MM-DD` で日付を上書きできる。

ライセンス: MIT
