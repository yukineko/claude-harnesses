---
name: flow
description: **ユーザーが明示的に起動したときだけ走る**（SessionStart の自動提案を廃止したため、こちらから提案することはしない）。課題の供給（compass の次の一手 / backlog のキュー / GitHub issues の mirror）から解決手段の実行までを1本のループで回し、問題解決が終わる（キューが空になる・指定課題が解決する）まで止まらずに進めたいときに使うこと。
argument-hint: "[任意: 直接の課題文。省略時は compass→backlog から自動でピックし、キューが空になるまで回す]"
allowed-tools: Task, AskUserQuestion, Bash(backlog:*), Bash(compass:*), Bash(condukt:*), Bash(fugu-router:*), Bash(hypothesis:*), Bash(overwatch:*), Bash(budgetguard:*), Bash(specguard:*), Bash(specforge:*), Bash(jq:*), Bash(printf:*), Bash(git:*), Read
---

```
SOURCE（課題の供給。上から順に引く）   EXECUTOR（解決手段の実行）
  backlog    … 確定済みキュー【主経路】─┐   ※各サイクル先頭で `backlog sync --apply`（GitHub issues と照合）
  hypothesis … 計測待ちの PDO 仮説      ├─▶  condukt（fugu-router がモデル選択）─▶ verify
  compass    … 次の右サイズの一手       │    ※backlog が空のときだけ引く（自動 carve）
  prompt     … ユーザー直の課題文      ─┘
                                     │
  specforge  … spec-gap の戻り経路   ◀┘  正典が無い課題は実装フェーズへ通さず
       └──▶ ratify（exit 9 / 12 なら理由を残課題に記録して次へ）
```

## 止まらないこと — 問題解決が終わるまで進める（ユーザー裁定 2026-11-21）

ユーザー裁定（逐語）:
「flowは問題解決がおわるまで止まることがないようにすること。問題解決とはbacklogなりissuesなりがなくなるまですすめること。」
「課題を指定されたときは、その課題が実現、解決されるところまですすむこと」

- `/flow` が自律的にループを止めてよい唯一の停止理由は、決定論の循環ブレーカー `condukt circuit check` の trip である（3-4 と下の「停止条件」を参照）。
- 予算（budgetguard）は情報であって停止理由ではない。予算逼迫・予算超過・判定不能はバッチ幅を N=1 に絞るだけで、ループは続ける。
- compaction（context の圧縮）は停止理由ではない。循環ブレーカーは compaction 軸を無効にして呼ぶ。
- 人間の判断待ちは停止理由ではない。自律モードでは各ゲートを `condukt policy answer --resolve` に通し、auto / deny / reject / cancel のどれかに必ず解決して進む（Step 0.5）。
- worker の blocked・spec-gap の divert・pivot 兆候・merge conflict・測れない決定・charter の彫り直しは、どれもループを止めない。その item を理由付きで pending / fail に残し、ループは次へ進む。
- ループを終えてよいのは「問題解決が終わった」とき — 引数なしならキュー（backlog の pending と、ピックできる measure / compass の一手）が空になったとき、引数ありならその課題が解決して検証を通ったとき — だけである。
- これを機構でも保証する: plugin の Stop hook `flow stop-gate` が、登録済み driver のセッションを、仕事が残る間は止めさせない（Step 2 の末尾を参照）。

## 自分から提案しない

**自分から `/flow` を提案してはならない。** 0.2.6 までは SessionStart hook が
「開いている仕事があれば AskUserQuestion で `/flow` を確認せよ」というディレクティブを
注入していたが（L2: propose-then-confirm）、ユーザーの指示で廃止した
（autoflow 側の同種の 2 経路も同時に撤去）。backlog に pending が積まれていることだけを理由に起動してはならない。
廃止したディレクティブを推測で再現してはならない。
（Stop hook の `flow stop-gate` は提案ではない。`/flow` を明示的に起動して driver 登録したセッションにしか作用しない。）

### Step 0 — 引数があれば、その課題が解決するまで進めること

乗り換えはしない。

`$ARGUMENTS` に課題文があれば Step 3（その課題文で condukt 実行）へ直行せよ。
ただしその前に Step 0.5 の自律判定と、Step 2 の driver 登録・Step 2.5 の flow-watch 武装を必ず行うこと（Step 1 と 3-1 のピックは飛ばす）。
引数の課題は 1 回実行して終わりではない。その課題が実現・解決され、condukt の完了ゲートと検証（verifier・再現テスト）を通ったと観測できるまで進めること。
検証を通らなかったら、失敗理由（failure_context）を添えて同じ課題を `/condukt` に再投入し、別のアプローチで解決まで進めること。

引数の課題は、Stop hook が解決まで session を止めさせないよう、hashkey を課題文から決めて claim すること（バッチ・backlog の item と同じ claim registry に載る）:

```sh
HK="arg-$(printf '%s' "$ARGUMENTS" | git hash-object --stdin)"   # 課題文から決まる hashkey（同じ課題文なら同じ値）
condukt state claim-task --run "flow-$CLAUDE_CODE_SESSION_ID" --session "$CLAUDE_CODE_SESSION_ID" \
  --stateless --title "<課題文の要約>" --hashkey "$HK"
```

- 検証を通らなかった試行ごとに、その claim を `--outcome` 無しで release してから同じ `$HK` で claim し直すこと（`condukt state release-task --run "flow-$CLAUDE_CODE_SESSION_ID" --hashkey "$HK"`）。
  失敗した試行が item-outcome log に記録され、3-4 の循環ブレーカーの failure streak がそれを数える（同じ失敗の無限反復は circuit が trip させる）。
- 解決して検証を通ったら `--outcome done` で release し（`condukt state release-task --run "flow-$CLAUDE_CODE_SESSION_ID" --outcome done --hashkey "$HK"`）、Step 4 へ進むこと。
- 各試行の後に 3-4 の heartbeat と循環ブレーカーを回すこと。

引数が「backlog を消化せよ」「キューを片付けよ」のようにキューの消化そのものを指すなら、それは特定の 1 課題ではないので、Step 1 → Step 2 → Step 3 の通常ループで backlog の pending が空になるまでループすること。
引数が空なら Step 1 へ進むこと。

どちらの経路でも、着手した課題が終わるまでそれだけをやること。
途中で見つけた別の問題は再現テスト（RED）を書いて `backlog add --repro-test "<cmd>"` で起票し、**その課題に戻ること**。
テスト無しの `backlog add` は拒否され store に何も書かれない。
下の「常時起票」を参照すること。

### Step 0.5 — 自律モードでは人間への問い合わせを `--resolve` で必ず解決すること

ループ中に人間へ問い合わせる（`AskUserQuestion`）箇所は、**自律モードでは各ゲート固有の
risk×reversible×confidence を `condukt policy answer --resolve` に渡し、決定論的な 4 つの結果
（auto / deny / reject / cancel）のどれかに従って進む**。自律モードで人間の返事を待ってループを止めてはならない。

ユーザー裁定（逐語）: 「PermissionのAskはすべてAllowにする。通常のAskのうち推奨があるなら、それを選ぶ」
「破壊はdeny,未測定はreject,autoで自答はいいが解決できないときは別の問いかcancel, 基本はすべてAllow」

まず `condukt state autonomy-check` で、そもそも縮退してよいかを確認せよ。

```bash
condukt state autonomy-check   # exit 0 + {"autonomous":true} → 自律 / exit 1 + {"autonomous":false} → 非自律
```

- **exit 1（非自律・既定）** → 従来どおり全ゲートで `AskUserQuestion` を出せ（`--resolve` は非自律では不活性で、escalate の exit 2 が返る）。
- `autonomy-check` が存在しない版や、exit 0 以外で終わる場合（存在しない版は unrecognized subcommand で exit 2 になる）→ **非自律とみなす**（安全側フォールバック＝全 Ask）。
- **exit 0（autonomous）** なら各ゲートを次の `condukt policy answer --resolve` に通せ。

```bash
# 各 human gate はまずこの shim を通すこと。exit code で 4 つの結果に分岐する
OUT=$(condukt policy answer --resolve \
        --risk <low|medium|high> --reversible <low|medium|high> --confidence <low|medium|high> \
        --question "<質問文>" --option "<A>" --option "<B>" --recommend <既定 index> 2>/dev/null)
case $? in
  0) CHOSEN=$(printf '%s' "$OUT" | jq -r '.chosen') ;;  # auto: 推奨の CHOSEN を採用して進む。自答は監査ログに追記される
  3) : ;;  # deny: 破壊的な操作。その操作だけ拒否し、記録して item を fail/excluded にし、ループは続ける
  4) : ;;  # reject: 未測定・測れない。item を完了と認めず backlog fail に理由を書き、ループは続ける
  5) : ;;  # cancel: 解決できない。別の答えられる問いとして 1 度だけ問い直し、だめなら item を理由付きで cancel/fail して続ける
  2) : ;;  # escalate: 非自律のときだけ返る。AskUserQuestion を出せ
  *) : ;;  # 上記以外（1=不正入力 / 127 など）は判定不能。自律なら reject と同じに扱い、非自律なら AskUserQuestion
esac
```

| 結果 | exit | 意味 | 次の動作 |
|---|---|---|---|
| **auto** | 0 | 基本はすべて Allow。推奨（`--recommend`）を自答した | stdout（`{"answered":true,"policy":"auto","chosen":"...","resolve":true}`）の `chosen` を採用して進む。Ask しない。自答は `gate-decisions.jsonl` に追記され `condukt policy answers` で監査できる |
| **deny** | 3 | 破壊的（base policy が block する組。risk=high かつ reversible=low など） | その操作を実行しない。理由を記録し、その item は `backlog fail`（作業前なら `release-task --outcome excluded`）にして、ループは次へ進む |
| **reject** | 4 | 未測定・測れない（`--untestable` / `--unmeasured`） | その item を完了と認めない。`backlog fail <id> --reason "<何が測れないか>"` で理由を残し、ループは次へ進む |
| **cancel** | 5 | 解決できない（`--conflict`、または有効な推奨が無い） | 推奨を付けた**別の答えられる問い**として 1 度だけ問い直す（もう一度 `--resolve` に通す）。それも auto にならなければ、その item を理由付きで cancel / fail してループは次へ進む |

- 自律モードでは exit 2（escalate）は返らない。`--resolve` を付け忘れると旧来の escalate（人間待ち）が戻るので、すべてのゲートに必ず付けること。
- 不正入力（exit 1）・`condukt` 不在（127）は判定不能であり、自律モードでは auto に丸めない。reject と同じく item を理由付きで fail にして次へ進む。
- 非自律モード（exit 1 の autonomy-check）のフォールバックとしてだけ `AskUserQuestion` を使ってよい。

#### 権限認可と判断要求を分けて渡すこと（どちらも `--resolve` に通す）

ユーザー常設許諾: 自律走行中の「進めてよいか」という YES/NO の権限認可は事前に許諾済み（2026-08-07）。
裁定 2026-11-21 により、権限認可の Ask はすべて Allow（auto）に解決する。
verdict をこの skill の中で hardcode してはならない。

権限認可のゲート（はい/いいえで答えられ、答えが「はい」だと分かっているもの）には、従来どおり `--approval` も付けること（非自律の旧経路でも自答されるように）。

```bash
condukt policy answer --resolve --approval \
  --risk <...> --reversible <...> --confidence <...> \
  --question "<進めてよいか>" --option "<進む>" --option "<やめる>" --recommend 0
```

判断要求（選択・設計判断）のゲートには `--approval` を付けない。`--resolve` が推奨を自答する。

1. **破壊は絶対に緩まない** — base policy が block する組（risk=high かつ reversible=low 等）は `--approval` を付けても `--resolve` で deny（exit 3）になる。
2. **非自律モードでは `--resolve` も `--approval` も不活性** — 自律判定はバイナリ側が行う。
3. **`--untestable` / `--unmeasured` は reject、`--conflict` は cancel** — `--approval` と併記しても自答しない。

| human gate | 種別 | risk | reversible | confidence | 追加フラグ | 自律の典型結果 | 結果ごとの動作 |
|---|---|---|---|---|---|---|---|
| **排他ロック競合**（Step 2・他セッションが明示的に `lock acquire` 済み） | 権限認可 | low | high | high | `--approval` | auto | 推奨＝**奪取しない**（`--force` 自動奪取はしない）。このセッションは driver にならず報告する（Step 2） |
| **resume 選択**（複数候補） | 権限認可 | low | high | high | `--approval` | auto | 3-1 の優先度 pick 規則の先頭 |
| **deploy/push の GATED 承認**（3-3 sink・Step 4） | 権限認可 | medium | medium | medium | `--approval` | auto | 承認して進む。deny なら その操作だけ行わず記録して次へ |
| **condukt Phase 3 の合意**（「この schedule で進む?」） | 権限認可 | schedule 由来 | high | schedule 由来 | `--approval` | auto | 提示した schedule のまま進む |
| **pivot-check**（Step 4・`pivot`） | 判断要求 | medium | high | low | なし | auto | 推奨＝**継続**。reason を報告に引用し、次の gap を取り直す |
| **worker が blocked**（condukt Phase 5） | 判断要求 | medium | medium | low | なし | auto / cancel | 推奨＝failure_context を添えて再投入。解決しなければ item を `backlog fail` に理由付きで残し次へ |
| **spec-gap divert**（3-1.5） | 判断要求 | medium | high | low | なし | auto | 推奨＝specforge へ divert（3値は固定） |
| **merge conflict の pick-a-side** | 判断要求 | — | — | — | `--conflict` | cancel | 別の答えられる問い（例: 両側を保つ統合案）で 1 度問い直す。だめなら item を fail にして次へ |
| **測れない決定**（CLAUDE.md §2） | 判断要求 | — | — | — | `--untestable` | reject | item を完了と認めず `backlog fail` に「何が測れないか」を書き、次へ |
| **charter 彫り直しの問い**（3-1 の 3） | 判断要求 | medium | high | low | なし | auto | 推奨案を採用して charter を保存し、一手を引く。cancel なら残りを残課題に記録して次へ |
| **循環ブレーカー trip**（`condukt circuit check`） | 決定論 | — | — | — | — | 人にも policy にも聞かない | 唯一の停止理由。ループを抜けて Step 4 へ |

### Step 1 — ここで compass / charter を引いてはならない

引数なしの `/flow` では、compass / charter を先に引いてはいけない。この Step で `compass gap` を実行してはならない。
確定キューである backlog を先に処理し、backlog が一手も出さなかったときだけ charter 側へ降りること。
Step 2（driver 登録）→ Step 3-1 へ進むこと。

### Step 2 — driver を非排他に登録すること

キュー全体をロックしない。
登録と解除を飛ばしてはならない。
同じ project のキューは複数セッションが同時に回してよい。
task 単位の排他は `backlog next --claim` が選択と予約を同一クリティカルセクションで行うことで保証される。

自分が driver であることを非排他に登録せよ（他の driver が居ても失敗しない）。

```bash
backlog driver register --session-id "$CLAUDE_CODE_SESSION_ID" --project "$PWD"
backlog lock status --project "$PWD"   # 参考: いま誰が driver か（drivers[] に全員が並ぶ）
```

> 登録は project ごとにスコープされる（`~/.backlog/drivers/<project のハッシュ>/<session>.driver`）。
> `--project` を付けずに `backlog lock status` を呼ぶと全 project 横断で driver の有無を返す。
> `/flow` は必ず `--project "$PWD"` を付けること。

- 他セッションが driver として登録済みでも、見送らない。並走して構わない。待つのは解ではない。
- **例外 — 誰かが明示的に排他ロックを取っている場合**（`backlog lock status` の `kind` が `exclusive-lock` で `stale` でない）は尊重し、Step 0.5 の policy-answer routing（`--resolve --approval`、`--risk low --reversible high --confidence high`）に通せ。
  - auto（exit 0）でも自動奪取はしない。`chosen`（＝奪取しない）を採用し、このセッションは driver にならない（`backlog driver unregister` して「排他ロック保持中のため driver にならない」と報告する）。排他ロックの保持者がそのキューの driver である。自答は監査ログに残る。
  - 非自律（exit 2）なら従来どおり
    `AskUserQuestion`（待機 / 強制奪取 `--force` / 中止）を出せ。
    `--force` は進捗に関わらず排他ロックを奪取する（`backlog lock acquire --force ...`）。
- `backlog lock status` が `undetermined`（registry を読めなかった）を返したら「driver 不在」とは読まない。安全側に倒し、理由を報告して見送ること。
- stale な driver 登録は次の `register` が自動で reap する（TTL 1800 秒・heartbeat 基準）。TTL 超過の排他ロックは進捗の停止が確認できたときだけ reap され、確認できなければ `--force` だけが奪取する。

#### Stop hook `flow stop-gate` — 登録した driver は仕事が残る間は止まれない

flow plugin は Stop hook `${CLAUDE_PLUGIN_ROOT}/bin/flow stop-gate` を同梱する。この hook は次のとおり判定する:

- このセッションが live な driver として登録されていて、`backlog next` が仕事を返す、またはこのセッションが `flow-<SESSION>` run の claim を保持している間は、stop を block する（ターンを続けさせる）。
- ただし `condukt circuit check`（`--no-compaction-axis` 付き）が trip していれば stop を許す（循環ブレーカーは唯一の停止理由であり、hook も同じ判定に従う）。
- 判定不能（registry・backlog・claims・circuit を観測できない）は、まず 1 回 block し、同じセッションで連続した次の判定不能の stop だけを、理由を表示したうえで allow にする（黙って通さない）。
- driver 登録の無いセッションは一切 block しない（`/flow` を起動していないセッションには作用しない）。
- Step 4 の `backlog driver unregister` が登録を消すと、以後 block しない。だから unregister は、キューが空になったとき・循環ブレーカーが trip したとき・引数の課題が解決して検証を通ったときにだけ行うこと。仕事が残っているのに unregister して抜けてはならない。

### Step 2.5 — flow-watch を Monitor で武装すること（停滞検知は機構で持つ）

「止まっていました」と後から報告するのは許されない。
停滞の検知を LLM の記憶に頼ってはならない。決定論的な `flow-watch` を Monitor で常駐させ、出力の各行を即座に処理せよ。
background agent の完了通知を待つことは、これらのイベント処理の代わりにならない。

```
Monitor: command = ${CLAUDE_PLUGIN_ROOT}/bin/flow-watch --session "$CLAUDE_CODE_SESSION_ID" --project "$PWD"
         timeout_ms = 1800000
```

- 監視対象の run は condukt 自身（`condukt state list-tasks`、cwd = `--project`）から取る。武装時点で既に走っていた他 driver の run は baseline として無視され、武装後に現れた run だけが監視対象になる。自分が起こした既存 run を明示的に監視したいときだけ `--run <run_id>` を足せ（繰り返し可）。`--condukt-state` は廃止されており渡すと exit 2 になる。
- Monitor は 30 分で切れる。`EXPIRED re-arm` の行、または Monitor の期限切れ通知が来たら、**同じ `--session` のまま**ただちに再武装せよ。既定の `--watch-state`（`${XDG_STATE_HOME:-$HOME/.local/state}/claude-harnesses/flow-watch/<SESSION>.json`）に監視対象が永続化されるので、再武装しても監視中の run は baseline に落ちない。`--watch-state` を変えたり消したりすると監視対象を失う。
- 各イベント（stdout の 1 行 = 1 イベント）には、その場で次のとおり反応すること:
  - `RELAY <id> agent=… cwd=…` / `RELAY-STALL <id> age=…s` → relay された command を一字一句そのまま main セッションで今すぐ実行し、結果を `SendMessage` で該当 agent に返せ。実行できないなら理由を添えて `askrelay dismiss` せよ。
  - `TASK-STALL <run_id> <task> status=… idle=…s` → その run / task（state・worktree のログ・担当 driver agent）を調べ、今すぐ unblock せよ。task 完了通知を待ってはならない。
  - `STATE <run_id> <task>=<status> …` → 完了した項目を sink へ書き戻せ（Step 3-3）。
  - `STATE <run_id> finished` → その run は condukt の一覧から消えた（全 task が終端 / run が閉じた）。結果を確認して sink へ書き戻し、後始末（worktree・branch の merge と削除）を進めよ。この行は 1 度しか出ない。
  - `HEARTBEAT-FAILED <which> rc=…` / `WATCH-ERROR …` → 行を逐語で報告し、原因を直せ。黙って流してはならない。

### Step 3 — 実行ループを回すこと

問題解決が終わる（キューが空になる・引数の課題が解決して検証を通る）まで、または循環ブレーカーが trip するまで以下を繰り返すこと。

> cwd は常にこのセッションの **linked worktree** にすること。
> `backlog`（add / done / fail / edit / cancel / `sync --apply`）と `compass`（charter --write / gap --write / outcome / opportunity add / evaluate）の **store 書き込みは、main（primary checkout）の作業ツリー内では拒否される**（非 0 終了・何も書かない）。
> 判定不能（git が答えない）も拒否に倒れる。
> 以下の `backlog` / `compass` はすべて、セッションの worktree を cwd にして実行せよ（`git worktree add …` で作った linked worktree）。
> 読み取り（list / pivot-check / `compass charter`（--write なし））は main からでも動く。
> `next`（--claim なし）と `compass gap`（--write なし）も main からでも動く。

#### 3-1. 次のタスクを優先度順にピックすること

各サイクルの最初に、backlog の store を GitHub issues と照合すること（ユーザー裁定 2026-11-21「backlogなりissuesなりがなくなるまですすめること」）。
cwd はこのセッションの linked worktree にすること（`sync --apply` は store 書き込みなので main では拒否される）。

```bash
backlog sync --apply   # store と GitHub issues の mirror を照合する（片方向: local が正。終わった task の issue を close、未 mirror の pending を issue 化、GitHub で close された issue は reopen 計画）
```

- mirror が無効な project では `backlog sync` は DISABLED の 1 行を出して何もしない。それはそのまま次へ進んでよい。
- `backlog sync` が非 0 で終わったら、出力を逐語で報告し、このサイクルは照合なしで次の手順へ進むこと（照合の失敗はピックを止める理由ではない）。store に変更が出たら commit すること。

続けて予算を情報として読むこと。予算はバッチ幅の判断にだけ使い、ピックそのものを止める理由にしない。
```bash
budgetguard check   # stdout は JSON 1 行。exit 0 = 予算内 / exit 1 = 予算超過 / exit 2 = 判定不能
```
- exit 0 かつ JSON の `pressure` が `false` なら、バッチ幅は通常どおり（下の 2 の a の N）。
- exit 0 かつ `pressure` が `true`（当日支出が `daily.warn_usd` 以上）なら予算逼迫なので、バッチ幅を N=1 に縮退して直列にすること。
- exit 1（予算超過）・exit 2（判定不能）・`budgetguard` 不在・起動失敗のときは、予算内とは読まない。バッチ幅を N=1 にしてピックを続けること。予算の状態は最終報告に逐語で書くこと。

compass / charter を先頭で引いてはならない。1 と 2 がどちらも一手を出さなかったときのフォールバックとしてだけ引くこと。
出荷しただけで仮説を validate してはならない（build ≠ validate）。

1. measure step として、新規 build より先に、出荷済み・未計測の仮説を回収せよ。
   ```bash
   hypothesis list --status awaiting-measurement   # 出荷済み・未計測（awaiting-measurement）の仮説を列挙する
   ```
   - 各 awaiting-measurement 仮説について、計測信号が今観測可能かを判定せよ。
     - 観測可能な場合、condukt を起動しない。実験で観測した成果を集め、3-3 の sink で `hypothesis validate/reject --evidence` して仮説を閉じよ（3-2 は飛ばす）。
     - まだ観測不能（データ蓄積待ち等）な場合、ここで無限ループしない。awaiting-measurement のまま残し、「計測待ち（まだ観測不能）」として報告して次の候補へ進むこと。
   - awaiting-measurement の仮説が 0 件なら次の候補へ進むこと。
2. measure 対象（今観測可能なもの）が無ければ backlog（確定キュー）を引くこと。
   backlog に複数の ready 課題がある場合は、順列（1件ずつ）ではなく 1 回の condukt run に束ねて並列処理せよ。
   並列・直列の判定を flow 側でしてはならない。独立候補を束ねて渡すだけにすること。
   ファイル競合・shared-glob・依存層は condukt が判定し、非衝突タスクだけを並列バッチに、衝突したものは直列に落とす。
   Gated クラスのタスクは単独で隔離され、バッチに入らない。

   （a）`backlog list` で覗いて上位 N 件を自分のものと決めてはいけない。バッチは 1 件ずつ `--claim` で予約しながら最大 N 件取り出し、cwd はこのセッションの linked worktree にすること。
      `--claim` の lease は repo 外の `~/.backlog/claims` に書かれ、main からでも拒否されない。
      ```bash
      backlog next --claim --project "$PWD"   # 選択と予約を同一クリティカルセクションで行う。N 回繰り返すこと。
      ```
      `next --claim` は選んだ task を untracked な claim ledger に lease として記録するため、どの checkout からでも 2 つの driver が同じ task を受け取ることは無い。
      lease のためにトラックされた `.backlog/tasks.toml` は書かれない。ただし候補の repro が再現しなくなっていれば、`next --claim` はその行を自動 cancel して（store を書き換えて）次の候補へ進む（ユーザー裁定 2026-11-13）。その store 変更は commit すること。main から実行すると自動 cancel が要る候補で claim が拒否される。
      `list` は lease 中の task を `claimed` と表示する。素の `next` はそれを返さない。
      - 出力が `no pending tasks` になるまで、または N 件（既定 N=condukt の `max_parallel` で、無指定なら 4。予算逼迫・予算超過・判定不能なら N=1）に達するまで繰り返すこと。
      - 各件の `id` / `title` / `notes` / `hashkey` を控えること（`hashkey` は `next --claim` の出力に含まれる）。N=1 の場合は従来どおり単一課題として扱うこと。
      - `no pending tasks` が返っても即座に「キューが空」と断定してはならない。1 度は取り直すこと。`next --claim` は台帳を読めなかったとき非 0 で終了し、stdout には何も出さない。
      「判定不能」を「claim なし」として続行してはならない。予約した各 item について `condukt state is-claimed --hashkey <hashkey>` を実行せよ（claim-skip ゲート）。
      判定は 3 値: exit 0 は claim 済みなのでその item をスキップする。exit 1 かつ stdout が `"claimed": false` なら claim なしなのでピックを続行する。それ以外（`condukt` が無い・exit 3＝claim registry を読めない・起動失敗・タイムアウト）は判定不能なので、claim 済みとみなしてその item をスキップし、理由（exit code / stderr）を報告すること。
      lease は保持者の heartbeat が続く間は 1 時間（`CLAIM_STALE_SECS`）を過ぎても除外し続け、heartbeat が止まって `CLAIM_STALE_SECS` を過ぎると除外をやめる。
   （b）予算逼迫、相互依存、同一領域・同一ファイルへの接触、deploy・push（Gated 相当）のいずれかが読み取れる候補は、バッチから外して 1 件ずつ直列に回すこと。
      判断に迷うものはバッチに入れてよい（condukt が衝突を検出して直列化する）。
   （c）このループが `backlog add` する場合、cwd はこのセッションの linked worktree にすること（main では拒否される）。compass opportunity 由来ならその weight を供給すること。
      ```bash
      W=$(compass gap | jq -r '.opportunities[0].weight // empty')
      backlog add --title "<課題>" --project "$PWD" --priority p1 --weight "${W:-0}" \
        --repro-test "<その課題を再現するコミット済みテスト>"   # 再現しなければ拒否され store に何も書かれない
      ```
      weight は同じ priority 内の順序だけを変え、priority は上書きしない。weight が同じなら従来の created_at 順になる。
      weight 無指定は既定 0.0 である。
      クロスプロジェクトで繰り返し検出される作業種別は `docs/backlog-tag-taxonomy.md` の規約タグ（例:
      `worktree-hygiene` / `deploy-verify` / `network-infra`）も併せて付ける。
   （d）`backlog next --claim` が予期しないエラー（台帳を読めない等）で終わったら、それを「キューが空」とは読まない。出力を逐語で報告し、3-4 の heartbeat と循環ブレーカーを回してから 3-1 を取り直すこと。
3. backlog が（1 度取り直したうえで）一手も出さず、measure 対象も無いときだけ compass / charter を引くこと。
   ```bash
   compass gap     # dod / measuring_stick / last_outcome / opportunities などを JSON で出す
   ```
   - **charter が鮮明で右サイズの一手が引ける**なら、`compass gap` の `opportunities` と `dod` からその一手を選び、6 へ進むこと。
     以降は backlog 由来と同じ扱いとし、sink には 3-3 の compass 由来（`compass outcome`）を使うこと。
   - charter が陳腐・矛盾・抽象すぎて一手が引けない場合、`/compass` を案内して終えてはならない。
     その場で charter を彫り直すこと。
     `compass` skill の Step 1〜5（`compass evaluate` → C3–C5 の自己判定 → 未解決の問いを 1 問ずつ詰める →
     `compass charter --write` → `compass gap --write "<gap>"`）を回し、彫り直した charter から一手を引いて 6 へ進むこと。
     - carve 中の各問いは、自律モードでは Step 0.5 の `condukt policy answer --resolve`（判断要求・`--approval` なし）に通し、
       推奨案（`--recommend`）を自答して進むこと。非自律モードでは従来どおり `AskUserQuestion` で 1 問ずつ聞くこと。
       ```bash
       condukt policy answer --resolve --risk medium --reversible high --confidence low \
         --question "<charter の未解決の問い>" --option "<推奨案>" --option "<対案>" --recommend 0
       ```
     - cancel（exit 5）なら推奨を付けた別の問いとして 1 度だけ問い直し、それも解決しなければ分かっている範囲で charter を保存し、彫り切れなかった残りを残課題として報告に書いて、ループは次のサイクルへ進むこと。
     - 非自律モードで人間が離席して `sentinel` が返った場合も同じく、分かっている範囲で charter を保存し、残りを残課題に記録してループは続けること（`sentinel` でループを止めない）。
       保留ストアは存在しない（compass の parked は route の JSON に印字されるだけで保存されない。2026-10-27 ユーザー裁定）。
   - `compass` バイナリが無い / 呼び出しが失敗した場合、ここで condukt へ流してはならない。
     判定不能を「仕事なし」にも「実行してよい」にも丸めてはならない。charter の状態を観測できなかった事実を逐語で報告し、このサイクルの compass は一手を出さなかった（判定不能）として 5 へ進むこと。

4. backlog が空になったことを理由に自動で open 仮説（新規 discovery）へ降りてはいけない。
   自動ループでは open 仮説は残課題として報告するだけにして 5 へ進むこと。
   open 仮説は、ユーザーが明示的にそれを回せと言ったときだけ引くこと。明示指示がある場合のみ以下を使うこと。
   ```bash
   hypothesis list --status open    # confidence 降順（同点 created_at 昇順）でソート済み。空なら次へ進むこと。
   ```
   `list --status open` の先頭（最高 confidence）から順にピックすること。
   open な仮説があれば、full build に直行する前に RAT ゲート（riskiest-assumption test）を通すこと。
   ```bash
   RAT=$(hypothesis rat <hid>)      # 未テストの最重要×弱証拠 assumption（leap of faith）を返す
   ```
   - `RAT` が非空（高リスク・未テストの leap of faith がある）なら、課題文は full build ではなく、
     その assumption だけを検証する最小 de-risk 実験にすること（"<assumption text> が成り立つかを最小コストで測る実験"）。
     `RAT` 行頭の index を控え、3-3 の sink で `hypothesis tested <hid> <index>` を呼んで計測ループを閉じること。
   - `RAT` が空（高リスクの未テスト assumption が無い＝既に de-risk 済み）なら、従来どおり
     その仮説を検証する実験（full build）を課題文にすること。
   いずれも仮説 ID を控えること。
5. 完了判定では、仕事が無いのに繰り返してはならない。下記の 3 つのどれかが一手を出す限りループを終えてはならない（pending が残っているのに早期に終えてはならない）。
   open 仮説だけを根拠にループを継続してはいけない。
   継続するのは、(i) backlog の pending（3-1 の 2）、(ii) measure step（3-1 の 1 で今観測可能な awaiting-measurement 仮説）、(iii) compass 主筋（3-1 の 3 のフォールバックで実際に引けた compass の一手）のどれかが実際に一手を出したときである。
   この 3 つがどれも空なら、問題解決が終わった（キューが空になった）ので Step 4 へ進むこと。
   残っている open 仮説と、まだ観測不能な awaiting-measurement は、残課題として報告するだけにしてループを終えること。
6. ピックしたタスクを課題文に組み立てること。
   - flow 側で並列上限の待ち合わせ制御をしてはならない。
   - 単一課題（compass 主筋 / measure / hypothesis / 単発 backlog）は、タイトル＋ notes（仕様・制約・参照ファイル）で 1 課題文にすること。
   - backlog バッチ（3 の a・b で複数残った場合）は、1 つの課題文に、各 backlog item を「独立した top-level タスク」として列挙すること。
     各項目に id・タイトル・notes・（分かるなら）触るファイルや領域を明記し、「これらは互いに独立であり、非衝突なものは並列に、
     衝突・共有リソースを触るものは直列に scheduleしてよい」と condukt に明示すること。
     item 単位で done_criteria を切ってもらい、item id ↔ condukt タスクの対応を控えること。
7. 課題文を組み立てた直後、選んだ source 種別によらず必ず overwatch に anchor を登録せよ。
   ```bash
   overwatch begin --key "<pdo-unit-id>" --title "<task title>" \
     --scope "<touched_files をカンマ区切り、不明なら省略>" \
     --done-criteria "<done_criteria>"
   ```
   - `<pdo-unit-id>` は backlog なら `hashkey`、compass 主筋 / measure / hypothesis なら move / 仮説 ID、引数の課題なら Step 0 の `$HK` など
     その PDO 単位を一意に指す文字列にすること。この key は Step 4 の `overwatch end` で解放するため控えておくこと。
   - `--scope` は分かるなら condukt の `touched_files` と同じ語彙（カンマ区切り）で渡すこと。調査・carve ループ中など
     scope が未確定なら省略すること。
     空 scope は「重なり無し」ではなく `scope_undeclared` として報告される。
   - バッチ（複数 backlog item）なら item ごとに `overwatch begin` を呼ぶこと（key = 各 item の `hashkey`）。

8. 選択を shared discovery store に記録せよ（バッチなら選んだ各 item を記録すること）。`select` は既に記録された行だけを selected にするので、先に `record` すること。
   ```bash
   compass discovery record --session-id "<SESSION_ID>" --title "<選んだタスクのタイトル>"
   compass discovery select --session-id "<SESSION_ID>" --title "<選んだタスクのタイトル>"
   ```

9. 着手前に、選んだタスク（バッチなら各 item）を claim せよ（TOCTOU の最終ガード）。
   ```bash
   condukt state claim-task --run "flow-$CLAUDE_CODE_SESSION_ID" --session "$CLAUDE_CODE_SESSION_ID" \
     --stateless --title "<title>" --hashkey <hashkey>
   ```
   `--stateless` を必ず付けること。
   `--stateless` を付けると condukt は owning session の transcript だけで生死を判定し、凍結していれば回収する。
   マーク無しの claim は run state 不在を「判定不能」として永久に保持する。
   exit 1（別セッションが直前に claim 済み＝スキップされた）ならその item は諦めて 3-1 に戻り次候補へ進むこと。

#### 3-1.5. 正典が無い課題は、condukt の実装フェーズへ進めず spec 起こしへ戻すこと

正典（spec）が無いと分かった課題を、そのまま condukt の実装フェーズへ進めてはならない。specforge へ戻すこと。


次の3つのどれか1つが立ったら、divert するか否かを下の合意ゲートに通すこと。

- trigger 1: condukt-researcher の research brief が `open_questions` を出したとき。3-2 の起動条件に従うこと。
- trigger 2: task が `confidence: low` を持つとき。condukt Phase 1 の Decomposition で取得し、3-2 の起動条件に従うこと。
- trigger 3: specguard brief が `verdict=not-covered` を返したとき。condukt 起動前の flow のプリフライトで取得すること。

##### trigger 3 は condukt を起こす前にプリフライトで測ること

```bash
BRIEF=$(specguard brief --json "<課題文の要約>" 2>/dev/null); rc=$?
```

`specguard brief --json` は三値（`covered` / `not-covered` / `undetermined`）を返し、exit code でも `undetermined`（10）と判定できた場合（0）を区別する。

- 判定不能を `not-covered` へ丸めてはならない。
- divert は書き込み側の動作なので、判定不能で divert を起こしてはならない。
- rc=0 かつ `verdict` が `covered` なら、divert せず 3-2 へ進むこと。
- rc=0 かつ `verdict` が `not-covered` なら、divert 候補として下の合意ゲートへ進むこと。
- それ以外すべて（rc=10 / specguard 不在 / 出力が読めない / verdict フィールドが無い）は `undetermined` として扱い、divert しないこと。
- 代わりに「正典の有無を観測できなかった」ことを残課題として報告し、その課題自体は通常どおり 3-2 へ進めること。

##### 合意ゲートに素の `AskUserQuestion` を使ってはならない。`condukt policy answer --resolve` を通すこと

判断要求なので `--approval` を付けてはならない。
trigger 1・2・3 のいずれかが立ったら、divert するか否かを Step 0.5 の policy-answer routing に通すこと。

```bash
condukt policy answer --resolve \
  --risk medium --reversible high --confidence low \
  --question "この課題は正典が無い (<発火した trigger>)。specforge へ戻すか、このまま condukt で実装するか?" \
  --option "specforge へ divert して spec を起こす" --option "このまま condukt で実装する" --recommend 0
```

risk・reversible・confidence の3値は固定値であり、ここで変えてはならない。
自律モードでは `--resolve` が推奨（divert）を自答する（auto, exit 0）。exit code の扱いは Step 0.5 の表に従うこと。
非自律モードでは escalate（exit 2）になり、従来どおり `AskUserQuestion` を出す。この経路を通さず
素の `AskUserQuestion` を直接出すことは禁止。

##### divert したら次の手順を踏むこと

- divert 回数を数えて打ち切るカウンタを足してはならない（同じ失敗の反復は循環ブレーカーの failure streak が見る）。
- 元の課題を `backlog fail` にしてはいけない。
- `specforge draft` に課題文を位置引数で渡してはならない（`error: unexpected argument`）。
- `backlog list --status pending` はピックに使ってはいけない。ピックは常に `next --claim` で行うこと。
- 差し戻し先が未設定の repo（`specforge.toml` が無い）でこの経路を踏んだら、divert を強行しない。その item を `backlog fail <id> --reason "spec-gap: 差し戻し先（specforge.toml）が無い"` にし、claim を release して、ループは次へ進むこと。
- 課題文は `--req <file>` か stdin で渡し、`--id` を必ず付けること。`ratify` には `--reason` を必ず付けること。
- 元の課題は `fail` せず pending のまま残すこと。`backlog edit <id> --status pending` は lease を解放せず、`fail` 済みの task の defer も解かない。このセッションが `backlog driver heartbeat` を続ける間、divert した課題の lease は失効せず、他の driver には配られない。heartbeat が止まって `CLAIM_STALE_SECS`（1 時間）を過ぎると再び配られる。
  divert した課題の condukt claim は `condukt state release-task ... --outcome excluded` で解放すること（作業はまだしていないので failure streak に数えない）。
- `ratify` が通らなければ（contract floor 違反で非0終了）、specforge が出す理由を残課題として報告に書き、その課題の claim を `--outcome` 無しで release してループは次へ進むこと。
- `ratify` は spec を批准したあと、`repro_test` を持つ要件ごとに `backlog add --repro-test <その repro_test>` を呼ぶ。
- `repro_test` の無い要件は起票されず、`ratify` はその要件を名指しして exit 12 で終了する（spec は批准済みのまま）。
- 本物の `backlog` は commit 済みの HEAD で `repro_test` が FAIL したときだけ起票する。`ratify` 自身が書いた `specs/<id>.toml` が未 commit のままなので、`ratify` の直後の `add` は全件拒否され exit 12 になる。批准済み spec を commit してから `specforge queue --id <spec id>` を実行すると起票される。
- `ratify` が exit 12 なら、起票されなかった要件とその理由を残課題として報告に書くこと。exit 9（キューを読めない・`backlog` が無い等、受け渡し自体の失敗）でも、spec 由来の要件が queue されなかったことを報告に書くこと。どちらもループは次へ進む。
- `ratify` は contract floor 違反（`Spec::contract_violations()` が非空）の spec の批准を拒否し、非0で終了する。

 この起動形は実測で通した（specguard 0.2.54、使い捨て git repo で draft→ratify→backlog を通しで実行し、`ratify` が `backlog: 3 件を起票 / 0 件は起票済み (計 3 requirement)` を出力し、直後の確認用の `backlog list --status pending`（ピックに使ってはいけない純粋な read）に `[r4demo:R1]`〜`[r4demo:R3]` の3件が `p1 / pending` で並んだ）。

```bash
printf '%s\n' "<課題文>" > "$req"
specforge draft --id <spec id> --title "<課題タイトル>" --req "$req"

specforge ratify --id <spec id> --reason "<なぜこの spec を受け入れるか>"
```

`specforge` は `specforge.toml` が無いと exit 2 で拒否する。

#### 3-2. 課題文を condukt で実行すること

- 分解は最後まで行わせること。保留するのは実装フェーズだけにすること。
- 1 セッション内で複数の condukt run を並走させてはならない。
- 課題文を `/condukt` に渡すこと。condukt が分解 JSON を出したら、`fugu-router route` が各タスクの `suggested_model` を実績から（Thompson sampling による確率的な選択で）上書きする（併用時）:

```
/condukt <課題文>
```

- `/condukt` は `Task` ツールで非同期起動すること。
- spec-gap の trigger 1 は condukt-researcher の research brief から、trigger 2 は Phase 1 の Decomposition から取ること（3-1.5 の続き）。課題文に「**research brief に `open_questions` が出たか、Decomposition（Phase 1〜2）のいずれかのタスクの `confidence` が `low` なら、Phase 5（実装）へ進まずその事実を報告して戻れ**」を明記して渡すこと。
- その報告で戻ってきたら 3-1.5 の合意ゲートへ入り、divert が選ばれたら specforge へ、選ばれなければ同じ Decomposition のまま `/condukt` を実装フェーズから再開させること。
- condukt の worker が blocked で戻ったら、Step 0.5 の表（worker が blocked）に従う。推奨は failure_context を添えた再投入であり、それでも解決しなければその item を `backlog fail` に理由付きで残して次へ進む（ループは止めない）。
- compass 由来の一手なら、`compass gap` の `measuring_stick` と `dod` を文脈として課題文に添えること。
- backlog バッチは 1 回の `/condukt` 呼び出しに束ねること。並列か直列かの実判定は condukt の `schedule.rs` が担う。

condukt 実行が完了したら、観測できた cost_usd・duration_secs を `fugu-router record` に `--delegation` を添えて記録し、直後に `audit-recent` でその記録が実際にストアへ着地したかを自己検証すること。
```bash
fugu-router record --title "<task title>" --class "flow-delegation" \
  --model <suggested_model> --status <verified|failed> \
  --cost <observed_cost_usd> --duration <observed_duration_secs> \
  --delegation <fork|inline>

fugu-router audit-recent --class "flow-delegation" --within 60
```
- `audit-recent` が exit 0（found:true）なら記録が確認できたので、通常どおり次のサイクルへ進むこと。
- `audit-recent` が exit 1（found:false）なら記録漏れなので、`fugu-router record` をもう一度試みること。再試行でも記録できなければ、ユーザーに「delegation 記録に失敗した」と明示的に警告すること。

#### 3-3. 検証が通ったら結果を source に書き戻すこと

- バッチ全体を一括で成功扱いにも失敗扱いにもしてはならない。
- 新規 experiment の出荷直後に、この場で `hypothesis validate` / `hypothesis reject` してはならない。

condukt の完了ゲートを通ったら結果を source に書き戻すこと。`backlog done` / `compass outcome` は cwd = このセッションの linked worktree で実行すること。`backlog fail` も同様に cwd = このセッションの linked worktree で実行すること。main の作業ツリーでは `backlog done` / `compass outcome` の store 書き込みが拒否される。

バッチ（複数 backlog item を 1 run に束ねた場合）は item ごとに個別 sink すること。6 で控えた item id と condukt タスクの対応を使い、通ったタスクの item は `done`、blocked/失敗の item は `fail` と書き分けること（部分成功をそのまま反映し、一部が失敗しても通った分は done にする）。

- 成功したら次のとおり sink すること。
  - backlog 由来なら、その item の再現テストを証拠にして `backlog done <id> --test "<再現テストのコマンド>" --red-rev <修正前の rev>` を実行すること（バッチなら通った各 item の id について実行）。
    素の `backlog done <id>` は拒否され、status は変わらない。
  - compass 由来なら、完了した move を measuring_stick で判定し、その verdict を記録すること。
    ```bash
    compass outcome --verdict <forward|unchanged|backward> --evidence "<観測した成果>"
    ```
    verdict は forward（前進）・unchanged（不変）・backward（後退）のいずれかとし、move の diff・テスト結果・gap への接近度から driver(LLM) が判定すること。
    `--evidence` には計測値（テスト数・ベンチ・観測した挙動）を必ず渡すこと。
    記録後 `compass gap` を取り直すと `last_outcome` が次サイクルに反映される。
  - hypothesis 由来（新規 experiment の build が完了）なら、linked_hypotheses を `awaiting-measurement`（出荷済み・未計測）へ遷移させるのは `condukt state gate` 自体ではなく、condukt skill が gate PASS 後に実行するブロックである。仮説は awaiting-measurement に残すこと。
    閉じるのは次サイクルの measure step（3-1 の 1）が観測値を添えて行うこと。「計測待ち N 件」を残課題として報告すること。
  - measure step 由来（3-1 の 1 で観測値を回収した awaiting-measurement 仮説）なら、観測した成果を添えて閉じること。反証なら `hypothesis reject <id> --reason "<反証内容>"` を使うこと:
    ```bash
    hypothesis validate <id> --run <RID> --evidence "<観測した成果>"
    ```
    これで awaiting-measurement から validated / rejected に遷移する。
    `hypothesis validate` / `hypothesis reject` は証拠を必須とし、観測値の無い「出荷だけ」では status を変えられない。
  - fugu-router 併用時は、検証結果（どのモデルが通ったか・コスト）を `fugu-router record` で書き戻すこと。
- 失敗（blocked / needs-serial / Step 0.5 の deny・reject・cancel 等）したら次のとおり sink すること。
  - backlog 由来なら `backlog fail <id> --reason "<概要>"` を実行し、スキップして次へ進むこと（バッチなら失敗した item だけ fail、他は上記どおり done）。
  - ユーザーに失敗を通知し、ループは続行すること。

sink が成功でも失敗でも、`backlog done` / `backlog fail` の直後（または中断時）に、その item の claim を解放すること。
`backlog done` した item は `--outcome done` を付けて解放すること:
```bash
condukt state release-task --run "flow-$CLAUDE_CODE_SESSION_ID" --outcome done --hashkey <hashkey>
```
`backlog fail` した item・中断した item は `--outcome` を付けずに（または `--outcome failed` で）解放すること:
```bash
condukt state release-task --run "flow-$CLAUDE_CODE_SESSION_ID" --hashkey <hashkey>
```
release-task は解放の前に item の outcome を run の item-outcome log に記録する。flow run には run state が無いので、3-4 の `condukt circuit check` はこの log の末尾から連続する done 以外の件数を failure streak として数える。`--outcome done` を付け忘れると成功した item も失敗として数えられる。
claim した後に除外した item（claim 後の重複再確認で他セッションとの overlap が判明した、作業せずに除外した、Step 0.5 の deny で作業前に外した、等）は `--outcome excluded` を付けて解放すること。excluded は failure streak に数えられず、直前までの streak をリセットもしない（done だけがリセットする）:
```bash
condukt state release-task --run "flow-$CLAUDE_CODE_SESSION_ID" --outcome excluded --hashkey <hashkey>
```
バッチなら item ごとに release すること（6/a で控えた `hashkey` を使う）。
別 run が保持している hashkey は解放されず、release-task は exit 1 で終わる。
claim を解放し忘れると、Stop hook `flow stop-gate` はこのセッションがまだ仕事を持っていると読み続ける。

#### 3-4. ループを継続してよいか判定すること

3-1 に戻る前に、保持中の claim を live に保つため定期的に heartbeat すること:
```bash
condukt state heartbeat --run "flow-$CLAUDE_CODE_SESSION_ID"
```
長時間のループでは各サイクルで呼ぶこと。

Step 2 で登録した driver 自体も同じサイクルで heartbeat すること:
```bash
backlog driver heartbeat --session-id "$CLAUDE_CODE_SESSION_ID" --project "$PWD"
```
driver 登録の staleness は heartbeat_at ベースの TTL（既定30分）で判定され、TTL を超えた登録は stale として reap される。

続けて決定論の循環ブレーカーを1本のコマンドで判定すること:
```bash
condukt circuit check --run "flow-$CLAUDE_CODE_SESSION_ID" --session "$CLAUDE_CODE_SESSION_ID" --no-compaction-axis   # trip なら nonzero、continue なら exit 0
```
nonzero（trip）なら人にも policy にも聞かず即 Step 4 へ進むこと。stop 理由 slug は JSONL に記録される。exit 0 なら 3-1 に戻ること。
`--no-compaction-axis` を必ず付けること。compaction（transcript の圧縮回数）は停止理由ではない（ユーザー裁定 2026-11-21）。長寿命 session でも、キューが空になるまで同じ session でループを続ける。

3-1 に戻ること。循環ブレーカーが trip したときだけ Step 4 へ進むこと（キューが空になったときは 3-1 の 5 のとおり Step 4 へ進む）。

### Step 4 — driver 登録を解除してサマリを報告すること

ここへ来るのは、問題解決が終わった（キューが空になった・引数の課題が解決して検証を通った）とき、循環ブレーカーが trip したとき、またはユーザーが直接中断を指示したときだけである。
どの場合も driver 登録の解除を飛ばしてはならない。

先に Step 2.5 で武装した flow-watch の Monitor を `TaskStop` で止めること（止め忘れると解除後も heartbeat を打ち続ける）。

次の解除を実行すること。ここでも cwd = このセッションの linked worktree にすること。main の作業ツリーでは `backlog` / `compass` の store 書き込みが拒否される:

```bash
backlog driver unregister --session-id "$CLAUDE_CODE_SESSION_ID" --project "$PWD"
```

unregister すると Stop hook `flow stop-gate` はこのセッションを block しなくなる。仕事が残っているのに（circuit trip でもないのに）ここへ来てはならない。

Step 2 で例外的に `backlog lock acquire` した場合のみ `backlog lock release --project "$PWD"` も打つこと。

3-1 で `overwatch begin` した各 `<pdo-unit-id>` について、対応する `end` を呼ぶこと:
```bash
overwatch end --key "<pdo-unit-id>" --status "<done|abandoned>"
```
- `--status` は sink の結果を反映すること（成功で閉じたなら `done`、失敗・未完で手放すなら `abandoned`）。
- バッチなら item ごとに `overwatch end` を呼ぶこと（begin と同じ key＝各 item の `hashkey` を使う）。

最後に「処理件数・成功・失敗（理由つき）・deny / reject / cancel した gate・残キュー・予算の状態・次に取り直した gap」を報告すること。
circuit trip で来た場合は、trip の reason slug と残キューを報告すること。

#### ループ終端で方向判断を行うこと

勝手に `/compass` を起動してはならない（彫り直しは保留する）。

driver 登録の解除直後、ループを正常終了した場合（中断・エラー以外）は以下を実行すること。

```bash
compass pivot-check   # {"recommendation":"persevere"|"pivot","streak":N,"threshold":N,"reason":"…"}
```

- `persevere` ならそのまま継続し、「次の gap を取り直す」と報告すること。
- `pivot` なら Step 0.5 の policy-answer routing に通すこと（判断要求・`--approval` なし）:
  ```bash
  condukt policy answer --risk medium --reversible high --confidence low --resolve \
    --question "pivot 兆候: north_star を彫り直す?" --option "再オリエンテーション" --option "継続" --recommend 1
  ```
  - auto（exit 0）なら `chosen`（推奨＝継続/persevere）を採用し、`reason` を報告に引用しつつ「次の gap を取り直す」で継続すること。自答は監査ログに残る。
  - 非自律（exit 2）・フォールバックなら、`reason`（streak 長・対象 verdict 列）を引用してユーザーに提示し、
    north_star を彫り直す（再オリエンテーション）か否かを問うこと（`AskUserQuestion`）、「再オリエンテーション」なら
    `/compass` を案内し、「継続」なら通常どおり報告すること。
- outcomes が 0 件なら pivot-check はスキップしてよい。

## 停止条件 — 唯一の停止理由は循環ブレーカー（circuit）

自律的にループを止めてよい唯一の停止理由は、決定論の循環ブレーカー `condukt circuit check`（`--no-compaction-axis` 付き）の trip である。

- `--session` を付け忘れてはならない。付け忘れると毎バッチ後に trip する。
- `--no-compaction-axis` を付け忘れてはならない。付け忘れると compaction の回数で trip し、仕事が残ったまま止まる。
- 判定不能を「仕事なし」にも「実行してよい」にも丸めてはならない。

ループの各イテレーション（3-4 の継続判定の一部）で `condukt circuit check --run "flow-$CLAUDE_CODE_SESSION_ID" --session "$CLAUDE_CODE_SESSION_ID" --no-compaction-axis` を実行すること。
trip しなければ exit 0（continue）になる。
trip を観測したら人にも policy にも聞かず即 clean stop して Step 4 へ進むこと。停止理由 slug は JSONL に記録される。
failure streak 軸: release-task が item-outcome log に記録した outcome の末尾から連続する done 以外の件数で trip する（excluded は数えない）。
idle 軸は fail-closed で、測れない idle は `idle_unmeasured` で trip する。
idle は live な stateless claim があれば、その所有 session の transcript の mtime から測る。
claim が無ければ、`--session` で渡した session の transcript の mtime から測る。
これは `--run` がちょうど `flow-<SESSION>` のときだけ有効である。
JSON の `idle_source` が `session_flag` になる。
transcript が見つからない場合は `idle_unmeasured` で trip する。
HOME 未設定・claim registry が読めない場合も `idle_unmeasured` で trip する。
非自律で追加確認を入れたい場合のフォールバックのみ、trip 後に `AskUserQuestion` で続行可否を聞いてよい。

次のものは停止理由ではない（ループは止まらない）:

- `budgetguard check` の結果（予算逼迫・予算超過・判定不能・不在）。バッチ幅を N=1 にして続け、予算の状態は最終報告に書く。
- compaction（transcript の圧縮）。
- 人間の判断待ち。自律モードでは Step 0.5 の `--resolve` が auto / deny / reject / cancel のどれかに必ず解決する。
- worker の blocked、spec-gap の divert、pivot 兆候、merge conflict、測れない決定、charter の彫り直しが終わらないこと。どれも止めない — item を理由付きで pending / fail に残してループは次へ進む。
- `compass` バイナリ不在・呼び出し失敗。観測できなかった事実を報告し、そのサイクルの compass を一手なしとして扱う。
- `backlog next` の予期しないエラー。「空」とは読まず、報告して 3-4 を回してから取り直す。
- `backlog sync` の失敗。報告して照合なしで続ける。

ユーザーが直接中断を指示したら（これはループの判断ではなくユーザーの指示である）、直ちに Step 4（driver 登録の解除）へ進むこと。

## 次のハードルールを守ること

- ループを終える（完了する）のは、backlog pending・measure・compass 主筋 の 3 つがどれも一手を出さなくなったときだけとし、仕事が無いのに繰り返してはならない。
  この 3 つがどれも一手を出さないなら Step 4 へ抜けること。`open` 仮説を継続理由にしてはならない。
  残った `open` 仮説と観測不能な awaiting-measurement は報告するだけにすること。
  pending が残っているのに早期に終えてはならない。3 つのどれかが一手を出す限りは終えないこと。詳細は Step 3-1 の 5 を見ること。
  引数の課題は、解決して検証を通るまで終えないこと（Step 0）。
- ユーザーの課題から逸脱してはならない。着手した課題は、終わるまでそれだけをやること。
  作業中に別の問題（より重要に見えるもの・途中で詰まった原因・気づいた欠陥）が現れても、そこへ乗り換えてはならない。
  判定基準は常に「いまユーザーの prompt に忠実か」とし、それが解決するまではその課題に注力すること。
  見つけた別課題は、その場で再現テストを書いて `backlog add --repro-test` し、戻ること。
  課題が完了してから、自律モードであれば次の backlog を処理すること。別課題を並行して処理してはならない。
  逸脱してよい唯一の例外は、その問題を解決しないと元の課題が物理的に前に進まない場合であり、そのときも「元の課題のために何を迂回しているか」を明示してから行い、済み次第すぐ戻ること。
- テストが無ければ起票してはならない（2026-10-04 ユーザー裁定:
  「testがないなら起票してはならない」）。気づいたことは、起票するかどうかを判断せず常に起票すること。
  起票は **(1) 再現テスト（RED）を書いてコミット → (2) `backlog add --repro-test "<cmd>"`** の順で行うこと。
  `backlog add` は再現した時だけ task を書き、それ以外は拒否して store に何も書かない。
  起票するかを人に聞かないこと（ユーザー裁定 2026-11-13「起票しますか?というのはもう聞かないこと。実証されたら起票、そうでないなら破棄。それは自動で実行していくこと」）。
  テストが書けない・`backlog add` が拒否した（再現しない・判定不能）ものは Ask せず**破棄し、報告に逐語の観測とともに列挙する**こと。
  保留（見送り）という状態は無い（同日裁定「保留はありえない実証されないならclose,実証されたらopen」。`backlog defer` は廃止）。
  `backlog next --claim` は払い出す前に候補の repro を再実行し、再現しなくなった行を**自動で cancel** して次の候補へ進む
  （インフラ要因で判定不能なら claim を拒否する）。自動 cancel はこのセッションの worktree の store を書き換えるので、
  その store 変更は他の変更と一緒に commit すること。
  「些細だ」「意図的な仕様かもしれない」「今回のスコープ外だ」を理由にこの手順を省かないこと。
  **それが本当に課題かどうかは、後でチケットから判定すること**。
  **起票の中身**には逐語の実測値・測定点（rev と日付）・判定不能なら**両方の読み**（(a) 実在の欠陥 / (b) 意図的な仕様）と、
  **どちらかを見分ける方法**を書くこと。
  起票を着手として扱わず、書いたら元の課題へ戻ること。
- source と executor の役割を混ぜてはならない。課題の選定は compass と backlog、実行は condukt に任せ、flow 自身は判定とループだけを担うこと。
- キューを独占してはならない。`backlog list` で覗いて上位 N 件を自分のものと決めるのは、純粋 read で重複着手するため禁止。
  待つのは解ではないので、他セッションが driver 登録済みでも見送ってはならない。
  `/flow` は同じ project で並走してよい（排他は task 単位＝`next --claim` + `condukt state claim-task`）。
- 複数 condukt run を並走させてはならない。並列はバッチを 1 condukt run に束ねて実現すること。
  backlog に複数 ready 課題があれば順列ではなく 1 回の condukt run に束ねて渡し、並列か直列かの実判定は condukt の `schedule.rs` に委譲すること。
  flow 自身は独立候補を束ねるだけにし、予算逼迫や明白な相互依存が読めるときは flow 側でバッチ幅を絞ること。極端な場合は N=1 にすること。
- 盲目実行してはならない。compass 由来の一手は、charter が鮮明でない限り condukt へ流してはならない（3-1 の 3 のとおり、彫り直してから引くこと）。
  backlog の item には、この charter 鮮度ゲートを適用しないこと。
- 循環ブレーカーの trip・ユーザー中断・エラー時も、driver 登録の解除を絶対に飛ばしてはならない。
- Step 0.5 のとおり、自律モードでは human gate を `condukt policy answer --resolve` に通すこと。`autonomy-check` が exit 0 のとき、
  各ゲートを per-gate の risk×reversible×confidence で `policy answer --resolve` に掛け、
  auto は推奨を自答（Ask 撤去・監査ログに追記）、deny は破壊的操作の拒否、reject は未測定として完了扱いにしない、
  cancel は 1 度だけ別の問いで問い直してだめなら item を cancel / fail、として扱うこと。どの結果でもループは次へ進む。
  循環ブレーカーは policy answer に通さず、決定論の `condukt circuit check` が trip すれば人にも policy にも聞かず clean stop すること（上記「停止条件」）。
- 判断を求める Ask には `--approval` を付けない（`--resolve` が推奨を自答する）。
  YES/NO の権限認可（排他ロック競合＝奪取しない、resume＝優先 pick 先頭、deploy/push の GATED 承認、
  condukt Phase 3 の合意）には `--approval` も付けること。常設許諾（2026-08-07）により auto で自答される。
  常設許諾も `--resolve` も破壊（deny）を緩めない。
  `--approval` と `--resolve` は非自律では不活性である。
  exit 1（既定・非自律）は従来どおり全 Ask を維持すること。
  `autonomy-check` が exit 0 以外なら非自律とみなすこと。
