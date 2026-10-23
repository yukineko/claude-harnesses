---
name: backlog
description: "既定で repo ごとの backlog（`<repo root>/.backlog/tasks.toml`）の queue+state を操作する backlog コマンド群（list / next / done / fail / driver / lock）の使い方。 ループ driver は /flow に統合された。"
argument-hint: [サブコマンド: list / next / done / fail / driver / lock]
allowed-tools: Bash(backlog:*), Bash(git:*), Read
---

> バックログのアイテムを順に実装したい場合は **`/flow`** を使ってください。

```bash
backlog list --status pending [--project <path>]   # ピックには使用禁止。キュー一覧（純粋な read）
backlog next [--project <path>]                    # 次のアイテムを覗く（予約しない）
backlog next --claim [--project <path>]            # 次のアイテムを予約して取る（driver はこちら）
backlog done <id> --test "<cmd>" --red-rev <rev>   # 完了（committed test が REV で RED・HEAD で GREEN を実測）
backlog done <id> --test "<cmd>" --red-rev <rev> --red-overlay <repro test file>   # 修正がテストファイルの中にあるとき: RED 側へ上書きする HEAD のテストファイルを repro 側だけに絞る（絞るだけで広げられない。REV で通るテストは拒否）
backlog done <id> --doc-only <commit>              # 完了（doc だけを触る祖先 commit が証拠）
backlog done <id> --duplicate-of <id>              # 完了（重複。相手は pending / claimed / done）
backlog add --repro-test "<cmd>" ...               # 再現した時だけ pending で起票し、それ以外は非 0 で拒否して store に何も書かない
backlog audit-open [--json]                        # open 行（pending/failed）に記録済み再現テストがあるかを、テストを実行せずに分類する
backlog audit-closures [--json]                    # 既存 close の証拠クラスを集計（read-only）
backlog fail <id> --reason "<概要>"                # アイテムを失敗マーク（2 日後に再キューされる）
backlog cancel <id> --reason "<理由>"              # やらないと決めて捨てる（終端。`discard` closure として理由を記録。証明できない推測はこれで捨てること。done と違い完了を主張しない）

# driver の存在通知（非排他。何セッションでも同時に登録できる）
backlog driver register   --session-id <id> --project <path>
backlog driver heartbeat  --session-id <id> --project <path>
backlog driver unregister --session-id <id> --project <path>
backlog driver status [--project <path>]           # 登録中の driver 一覧（active / count / drivers）

# 排他ロックは「全セッションを締め出す」ときだけ取ること。キューを回すのに取る必要は無い
backlog lock status [--project <path>]             # 排他ロック + driver 登録の合算ステータス（project 省略時は全 project 横断）
backlog lock acquire --session-id <id> --project <path>
backlog lock release --project <path> [--session-id <id>]   # 保有セッションだけが解放できる（既定は CLAUDE_CODE_SESSION_ID。他セッション保有・session id 無し・読めないロックは非 0 で拒否しロックを残す）
```

- `lock acquire` は**人が意図的に全セッションを締め出すときだけ**使うこと。保有中（heartbeat が stale でも）は保有セッション以外の `next --claim` が非 0 で拒否される。
- キュー操作は必ず repo の中から実行すること。
- `store_dir` を pin していなければ、repo root が上に無い cwd では `add` / `list` / `next` は拒否し、`~/.backlog` へフォールバックしない。
- **キューを回すのに排他ロックは要らない**。
- `next --claim` が選択と予約を同一クリティカルセクションで行うので、複数セッションが同じ project のキューを同時に消化しても同じ task を二重に掴むことはない。
- driver 登録も排他ロックも **project ごとにスコープ**される。
- `--project` は `status` 以外必須。
- `status` 省略時は全 project 横断スキャンになる。
- store は repo ごと（`<repo root>/.backlog/tasks.toml`）なので、`list` / `next` の `--project` は絞り込みではなく「どの store のことを言っているか」の assertion であり、別 repo を指すとエラーになる（絞り込まれた一覧は返らない）。
- この repo のタスクは全部スコープ内なので、通常は `--project` を **付けなくてよい**。

## 起票の基準 — テストが無ければ起票しない

- 見つけたら**まず再現テスト（RED）を書いてコミット**し、`backlog add --repro-test "<cmd>"` で起票する。
  `<cmd>` が HEAD で振る舞いとして失敗（＝再現）した時だけ task が `pending` で書かれる。
- それ以外は非 0 終了で拒否し、store に何も書かない: `--repro-test` 無し、再現せず、判定不能。
- `next` / `next --claim` は reproduced な repro の無い行を渡さない。
- status が `unconfirmed` / `needs-ruling` の行を含む store は `list` / `next` が拒否し、その行を名指しする。
- `backlog audit-open [--json]` は pending / failed の各行が **記録済みの** 再現テストを持つかを
  `has-test` / `no-test` / `undetermined` で返す。
- `audit-open` はテストを実行しない。
- 判定済みは `~/.backlog/audit-open/<project-slug>.json` に記録され、以後読み飛ばす。
- undetermined の行は次回また見る。
- 記録が読めなければ失敗する。
- `cancel` は空でない `--reason` が要る。
- `edit --status done|cancelled` は拒否される。

## コマンドを用途で使い分けること

- キューを確認したいときは `backlog list --status pending` を使うこと。
- 次のアイテムだけ確認したいときは `backlog next` を使うこと。着手するなら `next --claim` を使うこと。
- 手動で完了したいときは `backlog done <id> --test "<cmd>" --red-rev <rev>` を使うこと。素の `done` は拒否される。失敗マークしたいときは `backlog fail <id>` を使うこと。
- やらないと決めて閉じたいときは `backlog cancel <id> --reason "<理由>"` を使うこと（テスト無しで `discard` として記録される）。
- キューを自動で全件消化したいときは **`/flow` を使う**こと。

## 失敗モードの対処に従うこと

- **`lock status` が `kind: undetermined` を返しても「driver 不在」と読んではならない**（安全側に倒して見送ること）。
- `backlog` が起動できないときは README の plugin 導入手順を案内すること。
- **他セッションが driver 登録中** → 競合ではない。並走してよい（task 単位の排他は `next --claim` が保証する）。
- **排他ロック競合**（`lock status` の `kind` が `exclusive-lock`） → 保有セッションを確認し、必要なら `--force` で奪取すること。
- `kind: undetermined` は backlog が registry を読めなかった状態である。
