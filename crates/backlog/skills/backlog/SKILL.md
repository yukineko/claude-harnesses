---
name: backlog
description: クロスプロジェクト backlog の queue+state を操作するコマンド群へのショートカット。ループ driver は /flow に統合された。
argument-hint: [サブコマンド: list / next / done / fail / driver / lock]
allowed-tools: Bash(backlog:*), Bash(git:*), Read
---

# /backlog — backlog queue+state へのショートカット

`/backlog` は **backlog binary が提供する queue・state 操作**（`list` / `next` / `done` / `fail` /
`driver` / `lock`）を呼び出すための薄いエントリポイント。

> **ループ driver（driver 登録 → `next --claim` でピック → /condukt → done/fail → 登録解除）は `/flow` に統合されました。**
> バックログのアイテムを順に実装したい場合は **`/flow`** を使ってください。
> `/flow` は compass ゲート・budgetguard・fugu-router によるモデル選択も含む上位互換 driver です。

## backlog binary が提供するコマンド

```bash
backlog list --status pending [--project <path>]   # キュー一覧（純粋な read。ピックには使わない）
backlog next [--project <path>]                    # 次のアイテムを覗く（予約しない）
backlog next --claim [--project <path>]            # 次のアイテムを予約して取る（driver はこちら）
backlog done <id> --test "<cmd>" --red-rev <rev>   # 完了（committed test が REV で RED・HEAD で GREEN を実測）
backlog done <id> --doc-only <commit>              # 完了（doc だけを触る祖先 commit が証拠）
backlog done <id> --duplicate-of <id>              # 完了（重複。相手は pending / claimed / done）
backlog ruling request <id> --kind judgment|untestable ...  # テスト不能・判断での close は人間の承認待ちへ
backlog ruling approve <id>                        # 人間が TTY で id を打ち返して承認する（agent は不可）
backlog add ... --repro-test "<cmd>"               # 再現した時だけ起票（pending）。テスト無し・再現せず・判定不能は起票せず exit 2、.backlog/unproven.toml に記録
backlog unproven search "<text>" [--json]          # 起票されなかった（未実証の）記録を検索。調べ直す前に必ず引く（読めない記録は exit 3）
backlog unproven list [--json]                     # 未実証記録の全件
backlog confirm <id> --repro-test "<cmd>"          # unconfirmed を再現テストで pending へ昇格／repro の無い旧 pending に再現テストを記録
backlog audit-open [--json]                        # open 行（pending/failed/unconfirmed）に記録済み再現テストがあるかを 1 行 1 回だけ確認（テストは実行しない）
backlog audit-closures [--json]                    # 既存 close の証拠クラスを集計（read-only）
backlog fail <id> --reason "<概要>"                # アイテムを失敗マーク（2 日後に再キューされる）
backlog cancel <id> --reason "<理由>"              # やらないと決めて捨てる（終端。`discard` closure として理由を記録。証明できない推測はこれで捨てる。done と違い完了を主張しない）

# driver の存在通知（非排他。何セッションでも同時に登録できる）
backlog driver register   --session-id <id> --project <path>
backlog driver heartbeat  --session-id <id> --project <path>
backlog driver unregister --session-id <id> --project <path>
backlog driver status [--project <path>]           # 登録中の driver 一覧（active / count / drivers）

# 排他ロック（「全セッションを締め出す」ときだけ。キューを回すのに取る必要は無い）
backlog lock status [--project <path>]             # 排他ロック + driver 登録の合算ステータス（project 省略時は全 project 横断）
backlog lock acquire --session-id <id> --project <path>
backlog lock release --project <path>
```

> **キューを回すのに排他ロックは要らない。** `next --claim` が選択と予約を同一クリティカル
> セクションで行うので、複数セッションが同じ project のキューを同時に消化しても同じ task を
> 二重に掴むことはない。`lock acquire` はキュー全体を独占するので、**人が意図的に全セッションを
> 締め出すときだけ**使う。
> driver 登録も排他ロックも **project ごとにスコープ**される。`--project` は `status` 以外必須
> （`status` 省略時は全 project 横断スキャン＝`daily` の起動判定用）。
>
> **キュー側の `--project` は意味が違う。** store は repo ごと（`<repo root>/.backlog/tasks.toml`）
> なので、`list` / `next` の `--project` は絞り込みではなく「どの store のことを言っているか」の
> assertion であり、別 repo を指すとエラーになる（絞り込まれた一覧は返らない）。この repo の
> タスクは全部スコープ内なので、通常は **付けなくてよい**。repo root が上に無い cwd では
> `add`/`list`/`next` は拒否する（`~/.backlog` へフォールバックしない）ので、キュー操作は
> 必ず repo の中から実行する。

## 起票の基準 — テストが無ければ起票しない（2026-10-04 ユーザー裁定）

> 「testがないなら起票してはならない」
> 「見つけたら テストを実装する、検証されたときには起票する、テストがかけない、証拠がない場合は起票はしてはいけない。ただし記録はのこし同一の内容を検討しないようにする」

- 見つけたら**まず再現テスト（RED）を書いてコミット**し、`backlog add ... --repro-test "<cmd>"` で起票する。
  `<cmd>` が HEAD で振る舞いとして失敗（＝再現）した時だけ task が `pending` で書かれる。
- `--repro-test` 無し・exit 0（再現せず）・判定不能（未追跡スクリプト・allowlist 外・タイムアウト等）は
  **task を書かず exit 2**。代わりに `.backlog/unproven.toml`（キューの外）に title/notes/試行結果/時刻を
  1 件追記し、`unproven: u-…` を出す。テストが書けないなら Ask する（自分の判断で起票しない）。
- **同じ内容を検討し直さない**: `add` は毎回、過去の未実証記録との一致を stderr に `unproven-match: <id>` で出す
  （一致なしは明示的に `no prior unproven record`、記録が読めなければ `UNDETERMINED` ＝「一致なし」ではない）。
  調査を始める前に `backlog unproven search "<語>"` を引く（日本語は部分一致で引ける）。
- **旧 pending**（repro 記録の無い task）は一括降格しない。`next` / `next --claim` の JSON に
  `repro_required: true` が付き、stderr にも `repro_required: <id>` が出る。着手前に RED テストを書いて
  `backlog confirm <id> --repro-test "<cmd>"` で記録する。書けなければ `backlog cancel <id> --reason "unproven: …"`。
- **unconfirmed**（旧仕様で作られた疑い）のうち作成から 7 日を超えたものは `list`（`expiry candidate`、JSON は
  `expiry_candidate: true`）と SessionStart で失効候補として出る。`confirm` するか `cancel --reason "unproven: …"`
  で閉じる（`fail` は unconfirmed を拒否する）。自動では閉じない。
- **監査は open 行だけ、1 行 1 回だけ**（2026-10-04 裁定「完了のテストはみなくていい。pendingにテストがあるかだけ
  確認すればいいし2度確認しなくていい」）: `backlog audit-open [--json]` は pending / failed / unconfirmed（claimed は
  リース中の pending）の各行が **記録済みの** 再現テストを持つかを `has-test` / `no-test` / `undetermined` で返す。
  テストは一切実行しない（add/confirm 時に記録された repro を読むだけ）。判定済み（has-test / no-test）は untracked な
  `~/.backlog/audit-open/<project-slug>.json` に記録され、以後の実行では読み飛ばす（後で `confirm` で repro が付いても
  再確認しない）。undetermined は記録せず次回また見る。記録が読めなければ失敗し、何も確認済みにしない。store は書かない。
- **closed 行は記録として扱い、何もしない**（2026-10-04 裁定「過去はあくまで記録としてあつかう…ただしcloseにするときに
  監査されていないといけない」）。監査は close する瞬間に、その close コマンド自身が行う: `done --test` は RED@red-rev /
  GREEN@HEAD の実測（判断だけの done は拒否）、`done --doc-only` は doc だけの祖先 commit の検証、`done --duplicate-of` は
  相手 id の存在と status、`cancel` は空でない `--reason`（何を試し何を観測したか／なぜテスト不能か。`"unproven: …"`）、
  判断・テスト不能の close は人間の `ruling approve`（TTY）。`edit --status done|cancelled` は拒否。判定不能はすべて拒否
  （非 0・store 不変）。記録された closure がその close の監査結果であり、後から読み直さない。`fail` は終端ではない
  （2 日後に再キューされる）。

## 使い分け

| やりたいこと | 使うコマンド |
|---|---|
| キューを確認したい | `backlog list --status pending` |
| 次のアイテムだけ確認したい | `backlog next`（覗くだけ。着手するなら `next --claim`） |
| 手動で完了 / 失敗マークしたい | `backlog done <id> --test ... --red-rev ...`（証拠必須。素の `done` は拒否）/ `backlog fail <id>` |
| やらないと決めて閉じたい | `backlog cancel <id> --reason "<理由>"`（`discard` として記録。テスト・ruling 不要） |
| `unconfirmed` を棚卸ししたい（証拠あり→テストを書いて pending／証拠なし→cancel） | **`/backlog:triage`** |
| **キューを自動で全件消化したい** | **`/flow` を使う** |

## 失敗モード

- **`backlog` コマンド不在** → README の plugin 導入手順を案内する。
- **他セッションが driver 登録中** → 競合ではない。並走してよい（task 単位の排他は `next --claim` が保証する）。
- **排他ロック競合**（`lock status` の `kind` が `exclusive-lock`） → 誰かが意図的に全セッションを締め出している。
  保有セッションを確認し、必要なら `--force` で奪取する（`/flow` が処理する）。
- **`lock status` が `kind: undetermined` を返す** → backlog が registry を読めなかった状態。
  **「driver 不在」とは読まない**（安全側に倒して見送る）。
