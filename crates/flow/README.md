# flow

```
SOURCE（課題の供給）                          EXECUTOR（解決手段の実行）
  compass     … 次の右サイズの一手             ─┐
  backlog     … 確定済みキュー（+ GitHub issues mirror）├─▶  condukt（fugu-router がモデル選択）─▶ verify
  hypothesis  … PDO 仮説の build / measure      │
  prompt      … ユーザー直の課題文             ─┘

出荷した仮説は condukt skill が gate PASS 後に awaiting-measurement へ遷移させ、次サイクルの measure step が
計測して validate/reject する（出荷 ≠ 検証 ＝ build ≠ validate）。
```

## Never-stop contract (user rulings 2026-11-21)

The verbatim rulings live in `skills/flow/SKILL.md` (section 「止まらないこと」); in short:

- **No argument**: every cycle starts with `backlog sync --apply` (reconcile the store with its GitHub
  issues mirror), then picks with `backlog next --claim`. The loop runs until the queue yields no work
  (backlog pending, measurable hypotheses, a compass move).
- **With an argument**: the given task is claimed (`condukt state claim-task --stateless` with a hashkey
  derived from the statement) and pursued until it is solved and verified (condukt completion gate +
  verification). An argument that asks to drain the backlog loops until the backlog is empty.
- **The only stop reason is a `condukt circuit check` trip** (always called with
  `--no-compaction-axis`). Budget pressure / over / undetermined only narrows the batch width to N=1;
  compaction is not a stop; human-judgment waits are gone.
- **Human gates**: in autonomous mode every gate goes through `condukt policy answer --resolve`, which
  resolves to exactly one of **auto** (exit 0, adopt the recommended `chosen`), **deny** (exit 3,
  destructive: refuse that action), **reject** (exit 4, unmeasured/untestable: not accepted as done),
  **cancel** (exit 5, unresolvable: re-pose once as a different question, else cancel/fail the item).
  None of them ends the loop. Non-autonomous runs keep the old `AskUserQuestion` fallback.

It does **not** lock the whole queue: several sessions may run `/flow` on the same project at once.
Exclusion is per task — `backlog next --claim` (pick and reserve in one critical section) plus `condukt state claim-task` — and each session registers itself non-exclusively with `backlog driver register`.

The full procedure is `skills/flow/SKILL.md`:

```
0. 引数分岐 — 課題文があれば claim して、その課題が解決・検証を通るまで condukt で進める
     （「backlog を消化せよ」型の引数は通常ループで backlog が空になるまで）
0.5. 自律ゲート — `condukt state autonomy-check`。自律モードなら各 human gate を
     `condukt policy answer --resolve`（auto / deny / reject / cancel）に通す
1. compass は先に引かない — backlog 優先。compass は backlog が一手も出さなかったときのフォールバック
2. driver 登録（非排他）— `backlog driver register`。キュー全体はロックしない
2.5. flow-watch を Monitor で武装（停滞検知）
3. 実行ループ — `backlog sync --apply` → budgetguard（情報。バッチ幅だけ） → ピック（`backlog next --claim`）
       → spec-gap ゲート → claim → overwatch begin → /condukt → 検証 → sink（done / fail）→ release
       → heartbeat → `condukt circuit check --run "flow-$CLAUDE_CODE_SESSION_ID" --session "$CLAUDE_CODE_SESSION_ID" --no-compaction-axis`（trip だけが停止）
4. 終了 — キューが空 / 引数の課題が解決 / circuit trip で `backlog driver unregister`
     + `overwatch end` + サマリ報告 + pivot-check（`--resolve`、推奨＝継続）
```

## Layout — a Rust crate with a Stop hook

flow is no longer skill-only. It is a workspace crate whose binary serves the Stop hook:

```
.claude-plugin/plugin.json     # plugin manifest
Cargo.toml / src/              # the `flow` binary (src/stop_gate.rs, decide.rs, probes.rs)
bin/flow                       # POSIX launcher → bin/flow-<os>-<arch>
bin/flow-watch                 # stall watcher run under Monitor (Step 2.5)
hooks/hooks.json               # Stop → ${CLAUDE_PLUGIN_ROOT}/bin/flow stop-gate
skills/flow/SKILL.md           # the /flow skill (drives the source→executor loop)
```

### `flow stop-gate` (Stop hook)

- Blocks the stop of a **live registered driver** session while `backlog next` has work or the
  session holds `flow-<SESSION>` claims — unless `condukt circuit check --run flow-<SESSION> --session <SESSION> --no-compaction-axis` trips.
- Undeterminable (registry / backlog / claims / circuit unobservable) → blocks once, then a visible
  allow on the next consecutive undetermined stop of the same session. A panic of the gate body
  blocks (fail-closed, `harness_core::gate::run::run_guarded`).
- Sessions that are not registered drivers are **never** blocked: the hook does not propose `/flow`.
- Step 4's `backlog driver unregister` ends it — the skill unregisters only when the queue is empty,
  the circuit tripped, or the argument task is solved.

**flow still never proposes itself.** Through 0.2.6 a `flow propose` SessionStart hook injected an L2
propose-then-confirm directive; it was retired on the user's instruction (2026-08-20), together with
autoflow's two paths that said the same thing. The Stop hook above acts only on sessions that ran
`/flow` and registered as a driver.

```text
# in Claude Code:
/plugin marketplace add yukineko/claude-harnesses
/plugin install flow@yukineko
```

## Tests

```sh
cargo test -p flow --test stop_gate        # the Stop hook's behaviour
cargo test -p flow --test skill_contract   # the SKILL/README prose contract (rulings 2026-11-21)
cargo test -p integration-tests --test flow_skill_queue_contract   # no whole-queue lock, no pick via `backlog list`
```

MIT
