---
name: condukt-loop
description: test→fix→test ループを自動で回すスキル。server/client/e2e の3サイクルに対応し、差分0件 (failure_count 不変) またはテスト全件パスで自動停止する。
argument-hint: --module <server|client|e2e> [--max-iters <N>]
allowed-tools: Task, Bash(condukt:*), Bash(git:*), Read, Write, Edit, Grep, Glob
---

# /condukt-loop — test→fix→test 自動ループ

`/condukt-loop --module <server|client|e2e>` で、テスト失敗→コード修正→再テストを
自動で繰り返す。テスト全件パス、または進捗ゼロ (failure_count 不変) で自動停止する。

## サイクル定義

モジュール種別によって実行ステップが異なる:

| `--module` | ステップ順 |
|---|---|
| `server` | deploy → test |
| `client` | build → test |
| `e2e` | build → deploy → test |

build / deploy コマンドは `~/.condukt/config.toml` の `[loop]` セクションで設定する:

```toml
[loop]
build_command = "npm run build"
deploy_command = "kubectl rollout restart deployment/api"
max_iters = 10
```

## CLI 呼び出し例

```bash
# server サイクル (deploy → test) を 1 イテレーションだけ実行し、上限 max_iters は --iteration <i> を渡したときに検査されて上限を超えた呼び出しは cycle を実行せず拒否される
condukt loop --module server

# client サイクル (build → test) の 3 回目、前回の failure_count は 4
condukt loop --module client --iteration 3 --prev-failures 4

# e2e サイクル (build → deploy → test)、最大イテレーション数を上書き
condukt loop --module e2e --max-iters 5
```

1 イテレーション分の出力 (JSON):

```json
{
  "iteration": 1,
  "module": "client",
  "failure_count": 3,
  "success": false,
  "stop": false,
  "stop_reason": "",
  "output": "<combined stdout+stderr>"
}
```

`stop=true` のとき `stop_reason` は以下いずれか:

| stop_reason | 意味 |
|---|---|
| `"all tests pass"` | failure_count が 0 になった (正常完了) |
| `"no progress: failure count unchanged"` | failure_count が前回と変わらなかった (差分0件で自動停止) |
| `"max iters reached: ..."` | `--iteration` が上限を超えたので cycle を実行せず拒否した (exit 1、failure_count は null) |
| `"max iters undetermined: ..."` | config.toml が読めず上限を決められないので拒否した (exit 1) |

## 動作フロー

```
開始
 │
 ▼
condukt loop --module <type> を実行 (1 イテレーション)
 │
 ├─ stop=true ──────────────────────────────────────────────► 終了
 │     └─ stop_reason をユーザーに報告する
 │
 └─ stop=false
       │
       ├─ failure_count == 0 ────────────────────────────────► 終了 (全件パス)
       │
       └─ failure_count > 0
             │
             ▼
           output (stdout+stderr) を参照して失敗テストを特定
             │
             ▼
           condukt-worker subagent に修正を依頼する (Task ツール)
             │
             ▼
           condukt loop --module <type> を再実行 (次のイテレーション)
             │
             └─ 上限超過は binary が stop=true で返す ─────────► 終了
```

## 手順

### Step 1 — 引数を受け取る

`$ARGUMENTS` から `--module` と `--max-iters` を解析する。
`--module` は必須。未指定の場合はユーザーに確認すること。

### Step 2 — ループ開始

```bash
condukt loop --module <server|client|e2e> --iteration <i> [--prev-failures <prev>] [--max-iters <N>]
```

JSON を stdout から受け取り、フィールドを読む。
`--iteration` には 1 から数えた回数を、2 回目以降の `--prev-failures` には前回の `failure_count` を渡すこと。

### Step 3 — 停止判定

`stop=true` であれば `stop_reason` をユーザーに伝えて終了する。

- `"all tests pass"` → 成功で終了
- `"no progress: failure count unchanged"` → **差分0件のため自動停止**。
  同じ failure_count が続いており修正が機能していないことを報告する。
- `"max iters reached: ..."` / `"max iters undetermined: ..."` → 上限による停止 (Step 5)。

### Step 4 — 失敗テストの修正

`stop=false` かつ `failure_count > 0` のとき:

1. `output` フィールドから失敗しているテスト名・エラーメッセージを抽出する。
2. `Task` ツールで **condukt-worker subagent** を起動し、以下を渡して修正を依頼する:
   - 失敗テストの出力 (抜粋)
   - 修正対象ファイルのスコープ
   - 現在のイテレーション番号
3. subagent が修正を完了したら Step 2 へ戻る。

### Step 5 — 強制終了

上限の判定は `condukt loop` が行う: `--iteration` が上限 (`--max-iters`、未指定なら config.toml の
`max_iters`) を超えると、cycle を実行せずに `stop=true` を返して exit 1 で終わる。
config.toml に `max_iters` が無いときの上限は 10。
このスキル側で回数を数えて打ち切る判断はせず、Step 3 と同じくその JSON に従って終了し、直前のイテレーションの failure_count とともに報告すること。

## サイクル別の使い分け

### server サイクル (`--module server`)

**deploy → test** の順で実行する。
アプリケーションサーバーのコード変更後、デプロイを経てインテグレーションテストを検証する場合に使う。
ビルドステップが不要な (スクリプト言語などの) サーバーサイド修正に適している。

```bash
condukt loop --module server --max-iters 8
```

### client サイクル (`--module client`)

**build → test** の順で実行する。
フロントエンドやクライアントライブラリのコード変更後、ビルドを経てユニット/コンポーネントテストを検証する場合に使う。
デプロイが不要な純粋なビルド+テストサイクルに適している。

```bash
condukt loop --module client
```

### e2e サイクル (`--module e2e`)

**build → deploy → test** の順で実行する。
フロントエンドとサーバーの両方を変更するような場合や、エンドツーエンドテストでシステム全体を検証したい場合に使う。
3ステップすべてを経るため最も重いが、最も広い範囲をカバーする。

```bash
condukt loop --module e2e --max-iters 5
```

## 不変条件

1. **差分0件での自動停止** — `no progress: failure count unchanged` が返ったら無条件に停止し、
   同じ修正を繰り返すループには入らない。
2. **max-iters の強制終了** — イテレーション上限の超過は `condukt loop` が `stop=true` で拒否する。
3. **subagent への移譲** — コード修正は必ず condukt-worker subagent (Task ツール) に委譲し、
   このスキル自体はオーケストレーションに徹する。
4. **JSON のみ信頼** — `condukt loop` の stdout JSON のみを判断基準にする。
   テキスト出力の独自解析で停止判定を行わない。
