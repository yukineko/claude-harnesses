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
backlog next --claim [--project <path>]            # 次のアイテムを予約して取る（driver はこちら）。払い出す前に repro を再実行し、再現しなければ自動 cancel して次へ（判定不能なら拒否）
backlog done <id> --test "<cmd>" --red-rev <rev>   # 完了（committed test が HEAD で GREEN を実測。REV は commit 解決と祖先判定だけして closure.red に executed = false で記録し、RED は実行しない: ユーザー裁定 2026-11-08）
backlog done <id> --doc-only <commit>              # 完了（doc だけを触る祖先 commit が証拠）
backlog done <id> --duplicate-of <id>              # 完了（重複。相手は pending / claimed / done）
backlog add --repro-test "<cmd>" ...               # 再現した時だけ pending で起票し、それ以外は非 0 で拒否して store に何も書かない
backlog audit-open [--json]                        # open 行（pending/failed）に記録済み再現テストがあるかを、テストを実行せずに分類する
backlog audit-closures [--json]                    # 既存 close の証拠クラスを集計（read-only）
backlog fail <id> --reason "<概要>"                # アイテムを失敗マーク（2 日後に再キューされる）
backlog cancel <id> --reason "<理由>"              # やらないと決めて捨てる（終端。`discard` closure として理由を記録。証明できない推測はこれで捨てること。done と違い完了を主張しない）
backlog recheck [--id <id>]                        # open 行の repro を再実行: 再現→open のまま／再現しない・実証されない→自動 cancel／判定不能→触らず非 0
# backlog defer は廃止（2026-11-13 裁定「保留はありえない実証されないならclose,実証されたらopen」）。常に非 0 で拒否される

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
- **保留は無い**（ユーザー裁定 2026-11-13「保留はありえない実証されないならclose,実証されたらopen」）。`defer` は常に拒否される。`edit --status ruled-deferred` も、legacy な ruled-deferred 行への `edit --status` / `fail` も拒否され、legacy 行は `recheck` で解決する（再現→pending、それ以外→自動 cancel）。
- **起票するかを人に聞かない**（ユーザー裁定 2026-11-13「起票しますか?というのはもう聞かないこと。実証されたら起票、そうでないなら破棄。それは自動で実行していくこと」）。気づいたら RED を書いて `add --repro-test` する。add が拒否した（再現しない・判定不能）・RED が書けなかったものは**破棄**し、報告に列挙する。Ask しない。
- 再実行の判定境界: reproduced（振る舞いの失敗）→ open／not-reproduced（exit 0 でテストが走った）・not-demonstrated（script exit 2 = 環境 unavailable、126/127、テスト 0 件、pytest 4/5、cmd がパース不能、script が HEAD に無い）→ 自動 cancel／undeterminable（timeout・シグナル・spawn 失敗・git 失敗・dirty tree・build 失敗など）→ 自動 cancel しない（claim は拒否、recheck は非 0）。
- `next --claim` / `recheck` の自動 cancel は store への書き込みなので linked worktree で行われ、その store 変更は commit すること。primary（main）では自動 cancel が要る候補に当たると claim が拒否される。

## コマンドを用途で使い分けること

- キューを確認したいときは `backlog list --status pending` を使うこと。
- 次のアイテムだけ確認したいときは `backlog next` を使うこと。着手するなら `next --claim` を使うこと。
- 手動で完了したいときは `backlog done <id> --test "<cmd>" --red-rev <rev>` を使うこと。素の `done` は拒否される。失敗マークしたいときは `backlog fail <id>` を使うこと。
- やらないと決めて閉じたいときは `backlog cancel <id> --reason "<理由>"` を使うこと（テスト無しで `discard` として記録される）。
- 「見送り」はもう記録しない（`defer` は廃止）。まだ再現するチケットは open のまま、再現しないものは `backlog recheck --id <id>` で自動 cancel するか、`backlog cancel <id> --reason "<理由>"` で閉じること。
- キューを自動で全件消化したいときは **`/flow` を使う**こと。

## 失敗モードの対処に従うこと

- **`lock status` が `kind: undetermined` を返しても「driver 不在」と読んではならない**（安全側に倒して見送ること）。
- `backlog` が起動できないときは README の plugin 導入手順を案内すること。
- **他セッションが driver 登録中** → 競合ではない。並走してよい（task 単位の排他は `next --claim` が保証する）。
- **排他ロック競合**（`lock status` の `kind` が `exclusive-lock`） → 保有セッションを確認し、必要なら `--force` で奪取すること。
- `kind: undetermined` は backlog が registry を読めなかった状態である。
