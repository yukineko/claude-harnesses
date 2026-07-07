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
to run, when to stop) is **LLM** judgment inside your `/flow` skill; state, locking, size
routing, and model selection stay in the **existing binaries** (`compass` / `backlog` /
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

`flow` is a **superset of `/backlog`** (it adds the compass freshness gate and multiple
sources on top). The two share backlog's lock, so they serialize and must not run
concurrently.

## The `/flow` loop

The skill drives the loop; the binary only injects the SessionStart proposal directive.

```
0. 引数分岐 — 課題文があれば source 選択を飛ばして condukt に直行（1 件だけ実行）
0.5. 自律ゲート — `condukt state autonomy-check`。自律モードなら各 human gate を
     `condukt policy answer`（risk×reversible×confidence の graded 判定）に通す（下記参照）
1. compass ゲート — `compass gap`。charter が陳腐なら自動実行せず /compass を促して停止
2. ロック取得 — `backlog lock acquire`（クロスセッション直列化）
3. 実行ループ — 優先度順にピック（claim-skip ゲート）→ 着手前に claim（TOCTOU ガード）→ /condukt → 検証 → sink
       ピック順: compass 主筋
                 → measure step（awaiting-measurement の仮説を計測して validate/reject で閉じる）
                 → `backlog`（複数 ready 課題は順列でなく 1 condukt run に束ね、並列/直列は condukt の
                    schedule.rs に委譲＝非衝突は並列・衝突/危険は自動で直列。sink は item ごと）
                 → 新規 open 仮説（RAT ゲートで leap of faith があれば最小 de-risk 実験、無ければその仮説を検証する実験を build）
       成功 sink: backlog done
                 / compass は `compass outcome` で measuring_stick 判定（前進/不変/後退）を記録
                 / hypothesis は出荷で awaiting-measurement、計測後に validate/reject（証拠必須）
                 / fugu-router に record
                 / いずれも claim を release、ループ中は heartbeat で claim を live に保つ
       失敗: backlog fail --reason …、スキップして次へ
4. ロック解放 — source が尽きる/予算超過/中断で `backlog lock release` + サマリ報告 + pivot-check
```

**盲目実行しない**: compass ゲートが鮮明でない限り自動でキューを流し始めない。
**ロック解放を絶対に飛ばさない**（早期脱出・エラー時も）。

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
| Lock contention (Step 2, live holder) | auto | stand down (report and clean-exit; never force-steal) |
| Resume pick (multiple candidates) | auto | the existing priority-pick order |
| Pivot check (Step 4) | **escalate** | — (always a human call) |
| Circuit-breaker trip (early exit) | auto | clean stop |

Regardless of autonomy mode, four things always stop for a human: **(a)** a blocked
worker escalation, **(b)** deploy/push's GATED approval, **(c)** the pivot decision, and
**(d)** any gate where `policy answer` itself returns escalate/block.

### Early-exit conditions

| 状況 | 対応 |
|---|---|
| ユーザーが中断を指示 | 直ちに Step 4（ロック解放）へ |
| 循環ブレーカーが trip（`condukt circuit check` が毎イテレーション failure-streak上限・予算超過・no-progress stall を判定） | 決定論的に clean stop（人にも policy にも聞かない hard stop）。非自律での追加確認は `AskUserQuestion` フォールバックのみ |
| budgetguard が予算超過を返す | ループ終了（予算軸は上の circuit check にも統合済み） |
| compass ゲートが再スコープを示す | ループを止め `/compass` を促す |
| `backlog next` が予期しないエラー | 報告して Step 4 へ |

## The hook

Deterministic, non-blocking, exits 0 on any error (a driver hook must never break a turn):

| Hook | Event | What it does |
|---|---|---|
| **`flow propose`** | `SessionStart` (startup/resume/clear) | injects an **L2 propose-then-confirm** directive: if this session has open work (a compass next move, open backlog items, or an unfinished condukt run), the agent proactively offers `/flow` with a single `AskUserQuestion`. It does **not** recompute task counts — compass `nudge`, backlog `session-start`, and condukt `restore` already inject their own state; `propose` just adds the directive that ties them together. |

## Subcommand surface

The binary is intentionally thin:

| Subcommand | Purpose |
|---|---|
| `flow propose` | SessionStart hook: inject the propose-then-confirm directive |

## Install

### As a Claude Code plugin (recommended)

The plugin bundles the hook (`hooks/hooks.json`), the `/flow` skill, and a prebuilt
binary — so it runs entirely on your Claude **subscription**, no API key.

```text
# in Claude Code:
/plugin marketplace add yukineko/claude-harnesses
/plugin install flow@yukineko
```

The hook calls `${CLAUDE_PLUGIN_ROOT}/bin/flow propose`. `bin/flow` is a small POSIX
launcher that selects the right per-platform binary (`bin/flow-<os>-<arch>`); if a host
has no matching binary it exits 0 silently and prints a one-line build hint to stderr.

> `flow` requires its sources/executor (`compass`, `backlog`, `condukt`, and optionally
> `fugu-router`) to be installed — it is the driver that binds them, not a standalone.

### Build from source

```sh
scripts/build-plugin-bin.sh flow                       # host platform
scripts/build-plugin-bin.sh flow x86_64-apple-darwin   # cross-target the Intel Mac build
git add bin/ && git update-index --chmod=+x bin/flow bin/flow-*
```

## Platform support

| Host | File | Status |
|---|---|---|
| Linux x86_64 | `bin/flow-linux-x86_64` | bundled |
| macOS Apple Silicon | `bin/flow-darwin-arm64` | bundled |
| macOS Intel | `bin/flow-darwin-x86_64` | built in CI on a macOS runner |

## Plugin layout

```
.claude-plugin/plugin.json     # plugin manifest (version 0.1.6)
hooks/hooks.json               # SessionStart=propose → ${CLAUDE_PLUGIN_ROOT}/bin/flow
skills/flow/SKILL.md           # the /flow skill (drives the source→executor loop)
bin/flow                       # POSIX launcher → flow-<os>-<arch>
bin/flow-<os>-<arch>           # prebuilt binaries
src/main.rs … Cargo.toml       # the Rust crate
```

## Development

```sh
cargo build -p flow
```

## License

MIT
