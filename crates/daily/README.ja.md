# daily

> Claude Code 向けの**1 日 1 回タスクランナー**。Rust 製の `SessionStart` フックが、**登録したタスクを暦日あたり最大 1 回**だけ実行し、何を走らせたかの要約をセッションに還元する。

## 目的

`daily` は、Claude Code の `SessionStart` イベントに配線された決定論的な Rust バイナリである。その責務は、「定期的に回す価値はあるが、セッション開始のたびに回すのは無駄」なコマンドを、**暦日（ローカル時刻）あたりちょうど 1 回**だけ走らせ、結果を非ブロッキングに会話へ注入することにある。

時刻ベース（cron）の発火はマシンやシェルに依存して脆いため、`daily` は代わりに**各暦日の最初のセッション**をトリガーにする。最初のセッションだけがコストを払い、その日の以降のセッションは静かにスキップする。

走らせるタスクは `~/.daily/config.toml` に**登録**する（[設定](#設定)参照）。各 `[[task]]` は `name` とシェル `command`（任意で `dir`）からなり、`sh -c` で 1 日 1 回実行される。**タスクを 1 つも登録していない場合**は、従来どおり組み込みの `security` タスク（`cargo deny check advisories bans sources licenses`）が走る。

当日分のタスクを実行したあと、`daily` は「今日走らせたタスク」の 1 行要約を注入する（成功・失敗の両方）。

```
📋 daily: ran security (ok), deploy-check (ok)
⚠️ daily: ran security (fail exit 1: error[…]: advisory …), notes-sync (ok)
```

- exit 0 → `name (ok)`
- exit 非 0 → `name (fail exit N: <最初の要点行>)`（`error` / `warning` / `RUSTSEC` 行を優先）
- コマンドを spawn できない → `name (error: …)`
- 当日実行すべきものが無い → 沈黙する

各タスクは `dir`（無ければセッションの `cwd`）で、`$CARGO_HOME/bin` を `PATH` の先頭に足して実行される（`~/.cargo/bin` が PATH に無くても `cargo-deny` 等の cargo サブコマンドが解決できる）。デフォルトの security タスクを機能させるには `cargo-deny` を別途インストールする必要がある（`cargo install cargo-deny`）。

実行結果は `additionalContext` としてエージェントに渡される**非ブロッキング**な情報であり、ターンを中断させることはない。また **subscription-native**（サブスクリプション完結）であり、API キーは不要、マシン外に何も送らない。フックは LLM を呼ばない決定論的バイナリである。

## どうして必要か

セキュリティ監査のような重めのチェックは、毎セッション走らせるとセッション開始の体感コストが積み上がり、結局オフにされがちになる。一方で完全に手動にすると、走らせること自体を忘れる。

`daily` はこの「毎回は重い／手動だと忘れる」というジレンマを、**1 日 1 回ゲート**で解く。各暦日の最初のセッションだけがコストを払い、その日の以降のセッションは静かにスキップする。これにより、誰も意識しなくても依存関係の脆弱性・ライセンス・banned crate などの監査が日次で回り続ける。

「今日もう走ったか？」という判定は、共有クレートの `harness-core::daily::DailyGuard` にある決定論ロジックが担う（タスク名ごとに独立）。

- 状態ファイル `~/.daily/state/<name>-daily.txt` に最後に実行した `YYYY-MM-DD` を保持する。
- `should_run()` は保存された日付が今日と異なるときだけ真になり、`mark_done()` が今日の日付を刻む。
- ゲートは時計の時刻ではなく**暦日（ローカル時刻）**を基準とするため、その日に何回セッションを開いても実行はちょうど 1 回に保たれる。

## どう使うか

### フック配線

`daily` の入口は単一のフックである。

| フック | イベント | 内容 |
|---|---|---|
| `daily session-start` | `SessionStart`（startup / resume / clear） | driver が稼働していなければ、有効かつ当日未実行の各登録タスクを実行し、`mark_done()` で日付を刻み、レポートに記録し、要約を注入する。常に exit 0。 |

### 作業中は邪魔しないゲート（driver-skip）

`daily` は「作業していない時間（downtime）」にメンテナンスを回すためのもので、作業の最中に割り込むべきではない。そこでタスク実行の前に、`/flow` や `/backlog` の **driver が backlog ロックを生きたまま保持しているか**を `backlog lock status` で確認する。

- **生きた driver がロック保持中** → 静かに skip。タスクは `pending today` のまま残り、**次に driver が居ないセッション開始時**に実行される。
- **ロックが空 / 死んだ（stale）プロセスが保持** → 通常どおり実行（クラッシュした driver が `daily` を永久に塞がない）。
- **`backlog` 未インストール** → fail-open: driver 基盤が無い＝driver 稼働なし＝実行する。

`skip_when_driver_active = false` にすると常に実行する。

### 失敗しても「実行した」扱い

各タスクの日次ゲートは**実行後に exit code に関わらず刻まれる** — 失敗したタスクも「今日実行済み」となり、翌日まで再試行しない。失敗は失われず、代わりにレポート（とセッション要約）に記録される。

### レポート

タスク実行ごとに 1 行の JSON が `~/.daily/reports.jsonl` に追記される（`{date, at, task, status, code, detail}`、`status` ∈ `ok`/`fail`/`error`）。`daily report` で確認する。

```sh
daily report                 # 今日の実行結果
daily report --date 2026-07-01
daily report --last 20       # 全期間から直近 20 件
```

```
daily report — 2026-07-03
  ✓ security         [10:31:04] ok
  ✗ deploy-check     [10:31:05] fail exit 1: error[…]: advisory …
```

### タスクの登録

`~/.daily/config.toml` を直接編集するか、CLI を使う。

```sh
daily add --name notes-sync --command "git -C ~/notes pull --ff-only"
daily add --name build-cache --command "cargo fetch" --dir /path/to/repo
daily list     # 登録タスクと「今日実行済みか」を表示
```

`daily add` は既存の内容・コメントを保ったまま `[[task]]` ブロックを追記し、重複した name は拒否する。

### サブコマンド

| サブコマンド | 目的 |
|---|---|
| `daily session-start` | SessionStart フック本体: 当日未実行の各登録タスクを実行する（driver 稼働中は skip） |
| `daily list` | 登録タスクと当日実行済みかを表示する |
| `daily report [--date <d>] [--last <n>]` | 過去の実行結果レポートを表示する（既定は今日） |
| `daily add --name <n> --command <c> [--dir <d>]` | `~/.daily/config.toml` に日次タスクを登録する |
| `daily install` | （未実装）フックを `~/.claude/settings.json` に追加する。現状は手動配線が必要 |

### プラグインとしての導入（推奨）

```text
# Claude Code 内で:
/plugin marketplace add yukineko/claude-harnesses
/plugin install daily@yukineko
```

フックは `${CLAUDE_PLUGIN_ROOT}/bin/daily session-start` を呼ぶ。`bin/daily` はプラットフォーム別バイナリ（`bin/daily-<os>-<arch>`）を選ぶ POSIX ランチャーで、対応バイナリの無いホストでは静かに exit 0 する。セキュリティタスクを機能させるには `cargo-deny` を別途インストールする必要がある（`cargo install cargo-deny`）。

### 設定

`~/.daily/config.toml`（任意 — config が無ければ「有効＋デフォルト security タスク」として扱われる）:

```toml
enabled = true                   # false にすると全日次タスクを無効化
skip_when_driver_active = true   # /flow・/backlog の driver がロック保持中なら skip（既定 true）

[[task]]
name = "security"         # 一意な名前（1 日 1 回の状態キーにもなる）
command = "cargo deny check advisories bans sources licenses"

[[task]]
name = "notes-sync"
command = "git -C ~/notes pull --ff-only"
dir = "/home/me/notes"    # 任意。省略時はセッションの cwd
```

- `enabled` は省略時 **true**。`enabled = false` でランナー全体を停止できる。
- `[[task]]` が登録タスク。**1 つも登録しなければ**組み込みの `security` タスクが走り、**1 つ以上登録すれば**それらだけが走る（security も欲しければ自分で追加する）。
- 各 `name` は一意でなければならない（タスク毎の日次状態キーになるため）。
