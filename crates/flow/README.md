# flow

> **Unified source→executor driver for Claude Code**, written in Rust.
> The **autopilot layer**: it binds the task *sources* ([compass](../compass) next-move,
> [backlog](../backlog) queue, [hypothesis](../hypothesis) PDO lifecycle) to the *executor*
> ([condukt](../condukt), model-routed by [fugu-router](../fugu-router)) in one
> human-on-the-loop loop.

There are two separable concerns in keeping an agent productive across a session:
**supplying the next problem** and **executing it**. `flow` treats them as orthogonal
and pipes one into the other:

```
SOURCE（課題の供給）                          EXECUTOR（解決手段の実行）
  compass     … 次の右サイズの一手             ─┐
  backlog     … 確定済みキュー                  ├─▶  condukt（fugu-router がモデル選択）─▶ verify
  hypothesis  … PDO 仮説の build / measure      │
  prompt      … ユーザー直の課題文             ─┘

出荷した仮説は condukt が awaiting-measurement へ遷移させ、次サイクルの measure step が
計測して validate/reject する（出荷 ≠ 検証 ＝ build ≠ validate）。
```

It is **subscription-native**: no API key. The loop control (which source to pull, when
to run, when to stop) is **LLM** judgment inside your `/flow` skill; state, task-level claims,
size routing, and model selection stay in the **existing binaries** (`compass` / `backlog` /
`condukt` / `fugu-router`). `flow` itself holds **no new state** — it only binds the
deterministic layers that already exist.

## Where it sits in the harness

| Concern | Owner |
|---|---|
| What is this for · what's the next move? | `compass` |
| What's the open queue? | `backlog` |
| What PDO hypothesis is open to build / awaiting measurement? | `hypothesis` |
| Decompose / schedule / run / done-gate a task | `condukt` |
| Which Claude tier clears it cheapest? | `fugu-router` |
| **Bind source → executor in a loop; decide when to stop** | **`flow`** |

`flow` is a **superset of `/backlog`** (it adds multiple sources and the compass fallback
on top). It does **not** lock the whole queue: several sessions may run `/flow` on the same
project at once. Exclusion is per task — `backlog next --claim` (pick and reserve in one
critical section) plus `condukt state claim-task` — and each session registers itself
non-exclusively with `backlog driver register`.

## The `/flow` loop

The skill drives the loop. There is no binary any more — see [The hook, retired](#the-hook-retired).

```
0. 引数分岐 — 課題文があれば source 選択を飛ばして condukt に直行（1 件だけ実行）
0.5. 自律ゲート — `condukt state autonomy-check`。自律モードなら各 human gate を
     `condukt policy answer`（risk×reversible×confidence の graded 判定）に通す（下記参照）
1. compass は先に引かない — 引数なしの /flow は backlog 優先（2026-09-23 裁定）。
     compass は 3-1 の backlog が一手も出さなかったときのフォールバック
2. driver 登録（非排他）— `backlog driver register`。キュー全体はロックしない。
     他セッションの driver が居ても見送らない。誰かが明示的に `backlog lock acquire` した
     排他ロックが生きている場合だけ尊重する
3. 実行ループ — ピック（`backlog next --claim` で選択と予約を同一クリティカルセクションで行い、
       claim-skip ゲート）→ spec-gap ゲート → 着手前に claim（`condukt state claim-task`、TOCTOU ガード）
       → overwatch anchor 登録（overwatch begin）→ /condukt → 検証 → sink
       ピック順: measure step（awaiting-measurement の仮説を計測して validate/reject で閉じる）
                 → `backlog`（主経路。複数 ready 課題は順列でなく 1 condukt run に束ね、並列/直列は
                    condukt の schedule.rs に委譲＝非衝突は並列・衝突/危険は自動で直列。sink は item ごと）
                 → backlog が空のときだけ `compass gap`（charter が陳腐ならその場で彫り直す）
                 （open 仮説はユーザーが明示指示したときだけ。自動ループでは残課題として報告のみ）
       成功 sink: backlog done
                 / compass は `compass outcome` で measuring_stick 判定（前進/不変/後退）を記録
                 / hypothesis は出荷で awaiting-measurement、計測後に validate/reject（証拠必須）
                 / fugu-router に record
                 / いずれも claim を release、ループ中は heartbeat で claim を live に保つ
       失敗: backlog fail --reason …、スキップして次へ
4. 終了 — source が尽きる/予算超過/中断で `backlog driver unregister`（例外的に lock acquire した
     場合のみ lock release）+ `overwatch end` + サマリ報告 + pivot-check
```

**盲目実行しない**: compass 由来の一手には今も charter の鮮度ゲートがかかる（backlog item は
起票時点で観測と done 条件を持つので対象外）。**driver 登録の解除を飛ばさない**（早期脱出・エラー時も）。

### PDO session anchor (`overwatch begin`/`end`)

Right after building a task's text in Step 3-1 — **regardless of which source was chosen**
(compass next move / measure step / backlog / open hypothesis) — flow calls
`overwatch begin --key <pdo-unit-id> --title <title> [--scope <csv>] [--done-criteria <dc>]`
so the session's current responsibility lands in the project-wide registry (visible via
`overwatch status`; DESIGN §4.2). This anchors even a **measure step that starts no condukt
run**. In Step 4 the matching `overwatch end --key <k> --status <done|abandoned>` closes the
anchor's lifecycle. A batch (multiple backlog items) begins/ends per item. Both calls are
**fail-soft**: if the `overwatch` binary is absent, flow skips them and continues — the same
policy as the existing condukt/backlog/compass fail-soft.

### Autonomy gate (`condukt policy answer`)

Every point in the loop where the skill would otherwise stop to ask the user
(`AskUserQuestion`) first passes through a **global autonomy switch**:

```bash
condukt state autonomy-check   # exit 0 = autonomous / exit 1 (default) = non-autonomous
```

- **Non-autonomous (default, exit 1, or the subcommand doesn't exist)** — unchanged,
  full backward compatibility: every gate below still asks the user.
- **Autonomous (exit 0)** — each gate is routed through `condukt policy answer` with its
  own risk × reversibility × confidence, which returns a deterministic verdict:
  - **auto (exit 0)** — self-answer the recommended option, log it to
    `gate-decisions.jsonl` (auditable via `condukt policy answers`), and do **not** ask.
  - **escalate (exit 2)** — fall back to the normal `AskUserQuestion` (this is where the
    pivot decision always lands — a genuine strategy call stays human).
  - **block (exit 3)** — refuse and stop, without asking anyone.
  - anything else (bad input, missing `answer` subcommand) fails safe to `escalate`.

| Gate | Typical verdict | What "auto" picks |
|---|---|---|
| Exclusive lock held (Step 2 — only when someone explicitly ran `backlog lock acquire`; parallel drivers never trigger it) | auto | stand down (report and clean-exit; never force-steal) |
| Resume pick (multiple candidates) | auto | the existing priority-pick order |
| Pivot check (Step 4) | **escalate** | — (always a human call) |
| Circuit-breaker trip (early exit) | auto | clean stop |

Regardless of autonomy mode, four things always stop for a human: **(a)** a blocked
worker escalation, **(b)** deploy/push's GATED approval, **(c)** the pivot decision, and
**(d)** any gate where `policy answer` itself returns escalate/block.

### Early-exit conditions

| 状況 | 対応 |
|---|---|
| ユーザーが中断を指示 | 直ちに Step 4（driver 登録の解除）へ |
| 循環ブレーカーが trip（`condukt circuit check` が毎イテレーション failure-streak上限・予算超過・no-progress stall を判定） | 決定論的に clean stop（人にも policy にも聞かない hard stop）。非自律での追加確認は `AskUserQuestion` フォールバックのみ |
| budgetguard が予算超過を返す | ループ終了（予算軸は上の circuit check にも統合済み） |
| compass フォールバックで charter を彫っても右サイズの一手が出ない / `sentinel` で離席 | ループを止め Step 4 へ（彫り直しを勝手に繰り返さない） |
| `compass` バイナリ不在・呼び出し失敗 | 「仕事なし」にも「実行してよい」にも丸めず、報告して Step 4 へ |
| `backlog next` が予期しないエラー | 報告して Step 4 へ |

## The hook, retired

**flow has no hook. `/flow` runs when, and only when, you invoke it.**

Through 0.2.6 a `flow propose` SessionStart hook injected an L2
propose-then-confirm directive every session the backlog had pending items: *"before
starting other work, ask the user with a single AskUserQuestion whether to start
`/flow`."* **It was retired on the user's instruction (2026-08-20)**, together with
autoflow's two paths that said the same thing — its SessionStart "バックログに N 件…
/flow で開始しますか？" and its Stop-hook arm that blocked every single turn with
"/backlog を実行してください".

The reason is what a nudge is worth. Two plugins were making the same request, one of
them on every turn, and repetition is not detection: the tenth "there are N items"
carries no information the first did not. Whether to drain the queue is the operator's
call, and typing `/flow` *is* that call.

flow is therefore a **skills-only plugin** now (the shape `scout` and `daily-report`
already ship) — no binary, no launcher, no hook, not a Cargo crate. The `/flow` skill's
contents are unchanged.

## Install

### As a Claude Code plugin (recommended)

The plugin bundles the `/flow` skill and nothing else — it runs entirely on your Claude
**subscription**, no API key, and there is no binary to build for your platform.

```text
# in Claude Code:
/plugin marketplace add yukineko/claude-harnesses
/plugin install flow@yukineko
```

> `flow` requires its sources/executor (`compass`, `backlog`, `condukt`, and optionally
> `fugu-router`) to be installed — it is the driver that binds them, not a standalone.

## Platform support

Nothing to build, so nothing to be platform-specific about.

## Plugin layout

```
.claude-plugin/plugin.json     # plugin manifest
skills/flow/SKILL.md           # the /flow skill (drives the source→executor loop)
```

Through 0.2.6 it also shipped `hooks/hooks.json`, `bin/flow` (a POSIX launcher),
`bin/flow-<os>-<arch>`, `src/main.rs` and `Cargo.toml`. All of it existed to serve
`flow propose`, and went with it.

## Development

Not a Cargo crate, so `cargo` is not involved in shipping it. The `/flow` skill's
queue-driving contract is still pinned as text by
`crates/integration-tests/tests/flow_skill_queue_contract.rs` (moved there from
`crates/flow/tests/`):

```sh
cargo test -p integration-tests --test flow_skill_queue_contract
```

## License

MIT
