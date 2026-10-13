# harness-status

> 🌐 [English](README.md) ・ **日本語**

**Claude Code 向けの統合 HOTL ステータスダッシュボード (Rust 製)**

harness の各プラグインはそれぞれ独自の状態ストアを持つ。harness-status はそれらを
横断して読み取り、human-on-the-loop の視点 — 介入すべきかどうかを一目で判断するために
眺めるもの — を 1 画面にまとめて表示する:

- **予算 (budgetguard)**: 今日の支出額とセッション数。`~/.budgetguard/state/ledger.json`
  から読む。
- **直近セッション (gauge)**: 直近 N 件のセッションの turn 数・トークン数・USD コスト。
  `~/.gauge/store/sessions/` から読む。
- **進捗ファイル (taskprog)**: カレントディレクトリの `.claude/progress.md` のプレビュー。
- **Stop-hook レイテンシ**: `~/.harness/state/hook-latency.jsonl` からのセッション単位の合計。
- **UserPromptSubmit 注入サイズ**: `~/.harness/state/inject-metrics.jsonl` からのターン単位の合計。
- **hook binary の健全性**: `~/.claude/settings.json` に登録された hook のうち、binary が
  ディスク上に無いもの。
- **PATH shadowing**: plugin cache の同名 binary を PATH 上で覆い隠している古いコピー
  (例: `~/.cargo/bin` に残った旧版)。

**read-only** であり、書き込みは一切せず、他プラグインのストアを集約するだけである。API キーは
不要で、必要なときに手動で (または `/status` コマンドから) 実行する単一バイナリである。

**活性化スコープ: 手動 (CLI 専用) ダッシュボード＋狭い可観測性 hook 1本。** harness-status は
統合された手動 human-on-the-loop 検査ダッシュボードであり、汎用の `SessionStart` ダッシュボード
hook は登録**しない** — 毎セッション自動でダッシュボード全体を注入すれば、まさにこのツール
(`hooks` / `inject`) と [ADR 0001](../ctxrot/docs/adr/0001-cross-harness-injection-budget.md)
が抑えようとしている always-on の注入/hook 予算を膨らませてしまうからである。唯一の例外として、
`SessionStart` hook (`hooks/hooks.json`) を 1 本だけ登録し、hook-binary health check
(`harness-status session-start`) を実行して、`~/.claude/settings.json` に登録された hook の
binary がディスク上に無い場合、plugin cache の binary が PATH 上の古いコピーに shadow されて
いる場合、または PATH-shadow スキャン自体が完了しなかった場合 (判定不能として報告) **のみ**
`additionalContext` を注入する。サイレントに壊れた hook は人間が手動チェックしない限り気づけない
類のものなので、手動 `/status` 実行任せにせず表面化させる。それ以外のときは無出力・無注入で
サイレントであり、ターンをブロックしない。

このチェックの範囲の限界: 読むのは `~/.claude/settings.json` だけである。プラグインが自身の
`hooks/hooks.json` で `${CLAUDE_PLUGIN_ROOT}` 経由で登録する hook は解決されないため決して
検出されず、`settings.json` が無い・パースできない場合は「hook なし」として読まれる — 沈黙は
全プラグイン hook の binary が揃っている証明**ではない**。3 つのスコープの分類体系と全プラグインの現在の分類については
[`docs/plugin-activation-scopes.md`](../../docs/plugin-activation-scopes.md) を参照。

## 出力

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

── Progress file (taskprog) ──────────────────────
  cwd: /repo
  /repo/.claude/progress.md
  │ # Progress
  │ ## Pending
  │ - specforge ⑤ worktree merge
```

## インストール (プラグイン)

```
/plugin install harness-status@yukineko
```

導入後、任意のセッションで `/status` を実行する。

## 手動インストール

```sh
cargo install --path .
harness-status            # フルダッシュボード
```

## コマンド

```sh
harness-status                       # フルダッシュボード
harness-status budget                # 今日の支出のみ
harness-status sessions --sessions 10  # 直近セッション (件数 N を指定)
harness-status progress              # 進捗ファイルのみ
harness-status hooks                 # Stop-hook レイテンシ集約 (予算モニタ)
harness-status inject                # UserPromptSubmit 注入サイズ集約 (予算モニタ)
harness-status plugins               # 全プラグインを活性化スコープで分類
harness-status hooks-health          # settings.json の hook のうち binary 欠落のもの
harness-status path-shadow           # plugin cache 版を覆う PATH 上の古い binary
harness-status --json                # 機械可読出力 (任意のサブコマンドに付与可)
```

`plugins` サブコマンドはモノレポをスキャンし、全プラグインを
**always-on** / **event-scoped** / **manual** に分類する
([`docs/plugin-activation-scopes.md`](../../docs/plugin-activation-scopes.md) を参照)。これは
dev/HOTL 用ツールであり、リポジトリのレイアウトから分類するため、チェックアウトから実行すること。

## 補足

- あるセクションが "not installed" と出るのは、そのプラグインのストアが存在しないだけで、
  エラーではない。
- `sessions` と `path-shadow` は「読めなかった」と「何も無い」を区別する: gauge のストアが
  読めない、または PATH スキャンが完了しない場合は `unknown — …` (`--json` では
  `{"status":"unknown",…}`) を出して exit 1 で終わる。
- それ以外のパネルはこの区別を**しない**: 存在するが読めない/パースできない `ledger.json` は
  支出 `$0.0000` として表示され、読めないレイテンシ/注入台帳は「記録なし」として表示される。
  意外なゼロは未検証として扱うこと。
- `harness-status hooks` は、Stop-hook の合計実行時間が `HARNESS_HOOK_LATENCY_BUDGET_MS`
  (デフォルト `30000`。数値として読めない値はデフォルトに戻る) を超えるセッションを警告する。
- `harness-status inject` は、UserPromptSubmit 注入の合計が `HARNESS_INJECT_BUDGET_CHARS`
  (デフォルト `20000`) を超えるターンを警告する。
- 日付はシステムクロックから導出する (chrono 非依存)。テスト時は `HARNESS_DATE=YYYY-MM-DD` で
  上書きできる。

## ライセンス

MIT
