# e2e: human-0 autonomy chain (`scout 施策 → backlog → condukt → verify done`)

This document covers **autonomy DoD #6**: demonstrating, in a reproducible way,
that once the autonomy switch (`condukt state autonomy-check`) is on, the
`scout → backlog → condukt → verify done` chain runs **with zero human
intervention** — no `AskUserQuestion` decision prompt is hit on the happy path.

There are two layers, and it is important to keep them honest and separate:

| Layer | What it proves | How it is exercised |
|-------|----------------|---------------------|
| **Deterministic control plane** (switch + gate contracts + queue wiring) | The chain *is permitted* to run human-0. | `scripts/e2e-autonomy.sh` — fast, no LLM. |
| **Full runtime** (real scout research, flow decomposition, condukt worker/verifier) | An actual autonomous run *produces* a verified change. | Manual procedure below — heavy, LLM-driven. |

`scripts/e2e-autonomy.sh` covers the first layer only, and says so out loud. It
never claims the second. This split avoids a "silent truncation" where a green
check is mistaken for a full end-to-end proof.

---

## 1. The deterministic script (`scripts/e2e-autonomy.sh`)

```bash
bash scripts/e2e-autonomy.sh
```

It is `set -euo pipefail`, idempotent, and cleans up its throwaway state
(a temp `$HOME` holding a throwaway git repo + linked worktree whose
`.backlog/` is the demo store, since backlog keeps one store per repo) via an
`EXIT` trap. No real backlog store and no `~/.condukt` is modified. Pinned by
`scripts/tests/e2e-autonomy-runs.sh`.

### What it verifies

1. **Autonomy switch** — `condukt state autonomy-check` returns exit `0` and
   `{"autonomous":true}`. If the subcommand is missing (`exit 127`) or autonomy
   is disabled, the script prints an explicit `SKIP` with remediation and does
   **not** pretend the chain ran.
2. **Backlog queue mechanics** — a scout-style opportunity is enqueued with
   `backlog add` and then surfaces in `backlog list --status pending`. This is
   the hand-off queue between scout and flow/condukt. Run in an isolated temp
   `$HOME` so it cannot pollute the real queue.
3. **Gate contracts** — greps the *installed* SKILL specs to assert every human
   `AskUserQuestion` stop on the chain degrades under autonomy:
   - **scout Phase 4** (opportunity-selection Ask) → skipped; auto-selects top N.
   - **scout Phase 6** (hand-off Ask) → `auto-handoff` straight into `/flow`.
   - **condukt Phase 3** (decomposition-agreement Ask) → `schedule` adopted as-is.
   - **flow Step 0.5** (lock / resume / pivot / 3-failure Asks) → degraded to
     deterministic defaults (stand-down / priority-pick / persevere / clean-stop).
4. **Human-0 assertion** — with all of the above, no human decision prompt
   remains on the autonomous happy path. The only stops left are the **safety
   invariants** (a worker reporting `blocked`, or a hard verifier failure), which
   are *reports*, not human decision gates.

### What it deliberately does **NOT** verify

- The **real LLM work**: condukt's worker/verifier generation, scout's web
  research/scoring, flow's decomposition reasoning. All heavy and model-driven —
  out of scope for a deterministic check.
- The **semantic correctness** of any generated code.
- **Runtime side effects**: git worktree creation, merges, `condukt` run state
  transitions from a real dispatch.

Passing means: *"the deterministic layer permits a human-0 chain."* It does
**not** mean *"a real autonomous run produced a correct, merged change."* For
that, run the manual full-e2e below.

---

## 2. Manual full e2e (with real LLM)

Prerequisites:

- Autonomy is on: `~/.condukt/config.toml` has `autonomous = true`
  (or export `CONDUKT_AUTONOMOUS=1` for a single session). Confirm with:
  ```bash
  condukt state autonomy-check   # expect: exit 0 + {"autonomous":true}
  ```
- The `scout`, `flow`, `condukt`, and `backlog` plugins are installed and their
  binaries are on `PATH`.
- A scratch git project to operate on (do NOT point this at a repo you care
  about; the run will create worktrees, commits, and branches).

Procedure (each step is a Claude Code session action, no human answer expected
once autonomy is on):

1. **scout** — run `/scout` against the scratch project.
   - *Expected*: scout researches, scores opportunities, and — because
     autonomy-check returns exit 0 — **skips the Phase 4 selection Ask**,
     auto-selecting the top N and `backlog add`-ing them. It then **skips the
     Phase 6 hand-off Ask** and auto-launches `/flow` (only when ≥1 item was
     enqueued; 0 items ⇒ no launch, by design).
2. **backlog** — the seeded opportunities appear in the queue.
   - *Expected*: `backlog list --status pending` shows the scout-added items.
3. **flow → condukt** — flow picks the source item and dispatches to condukt.
   - *Expected*: flow's **Step 0.5** degrades its human gates (lock conflict ⇒
     stand-down, resume ⇒ deterministic pick, pivot ⇒ persevere, 3 failures ⇒
     clean stop) — no `AskUserQuestion`. condukt's **Phase 3** adopts the
     `schedule` output without an agreement Ask and proceeds to dispatch workers.
4. **verify done** — condukt runs worker → verifier → gate.
   - *Expected*: on success, `condukt state gate <run>` reports the run complete
     and the change is verified/merged per the normal completion gate. The run
     reaches a terminal `verified` state **without a human prompt**.

Success criteria for the manual run:

- No `AskUserQuestion` prompt was presented at any point (0 human interventions).
- The backlog item flows from `pending` → dispatched → `verified`/`done`.
- The only halts, if any, were safety invariants (worker `blocked` / hard
  verifier failure), which are surfaced as reports, not decision prompts.

---

## 3. DoD #6 satisfaction

DoD #6 is satisfied when **both** hold:

- **Deterministic layer** — `scripts/e2e-autonomy.sh` is green (switch on, queue
  wiring works, every chain gate contract degrades under autonomy). This is the
  reproducible, CI-friendly proof.
- **Full runtime layer** — the manual procedure above completes a real
  `scout → backlog → condukt → verify done` cycle with zero human interventions.

The script is the fast, always-runnable guardrail; the manual procedure is the
periodic real-world confirmation. Keeping them explicitly separate is what keeps
the claim honest.
