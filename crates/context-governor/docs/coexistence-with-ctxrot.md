# Coexistence with `ctxrot`

`context-governor` (CG) and [`ctxrot`](../../ctxrot/README.md) both register Claude
Code hooks, and they overlap on four events: **PostToolUse**, **UserPromptSubmit**,
**SessionStart**, **PreCompact**. This document records *why the two compose without
conflict* on every shared event, and the intended ordering where it matters.

Every claim below was verified against the handler source, not assumed. The
verifying file is named for each event so the matrix can be re-checked when either
plugin changes. An integration test
(`tests/context_governor_coexistence_with_ctxrot.rs`) locks in CG's half of the
contract: that it emits a valid envelope and exits 0 on every shared event (the one
exception being a `PreCompact` *Block*, which CG's defaults never produce), without
depending on or clobbering any `ctxrot` state.

## The two levers are orthogonal

The plugins are not two implementations of the same thing — they pull on different
levers, which is the root reason they compose:

| | **context-governor** | **ctxrot** |
|---|---|---|
| Role | the **size / cost / correctness** lever | the **rot-detect / rescue / control** lever |
| Acts by | **mutating** the window: groom tool results, inject reference bodies, rehydrate resident norms, snapshot on compact, checkpoint completed work | **nudging / routing / rescuing**: band advice, large-ref steering, rescue notes, a load gate, plus the `/ctx` and `/distill` skills |
| Writes to | `updatedToolOutput` (groom), `additionalContext` (inject/rehydrate), its own action ledger + backing store | stdout text (guard/restore), `hookSpecificOutput.additionalContext` (toolguard), a `deny` decision (preguard only), durable rescue/distill notes |
| Can block a turn? | **only** `PreCompact` may exit 2, and the default `CompactionGuard` never does (it always `Proceed`s) | **only** `preguard` (PreToolUse) can `deny`; every other ctxrot hook exits 0 |

Because CG *mutates structured output fields* while ctxrot *adds advisory text or
side-effect notes*, their writes land in different places and neither reads the
other's state. The composition is field-disjoint, not coordinated.

## Per-shared-event decision matrix

### PostToolUse — they write **different fields**, so they compose

- **CG** (`src/defaults/groomer.rs`): when a tool result exceeds the (pressure-aware)
  token budget, the groomer head/tail-trims the body and emits a
  `hookSpecificOutput.updatedToolOutput` envelope (`HookOutput::groomed`). It only
  ever *shrinks* an `Evictable` body (invariant I4, property-tested:
  `groom_never_grows_the_window`); under budget it emits `{}`.
- **ctxrot** (`src/hooks/toolguard.rs`): on a watched tool (`Read|Bash|Grep|Glob|
  WebFetch|BashOutput|NotebookRead`) whose response exceeds `huge_tool_output_bytes`,
  it emits a `hookSpecificOutput.additionalContext` **nudge** ("route the next heavy
  read through a sub-agent"). The module doc states it explicitly: *"We do NOT block
  the tool."* It never sets `updatedToolOutput`.

**Why no conflict:** `updatedToolOutput` (CG, the in-place rewrite) and
`additionalContext` (ctxrot, the steering text for the *next* turn) are distinct
fields of the PostToolUse envelope. CG shrinks *this* result; ctxrot advises about
the *next* read. They are complementary halves of "this output was too big". Ordering
is irrelevant: neither reads the other's field.

### UserPromptSubmit — both only **add** context, never replace the prompt

- **CG** (`src/defaults/injector.rs`): heading-addressable retrieval. Scores spec
  sections against the prompt and emits an `additionalContext` envelope with the
  matched section (or a table-of-contents sentinel). The module doc and the property
  test `inject_for_never_panics_or_replaces_prompt` guarantee it *only adds* context
  — it never emits `updatedToolOutput` and never replaces the prompt. Requires
  `CONTEXT_GOVERNOR_REFERENCE_DOC`; unset → `{}`.
- **ctxrot** (`src/hooks/guard.rs`): detects large references in the prompt and
  context-budget band crossings, and prints minimal, conditional advice to stdout
  (injected as additional context). At band ≥ 2 it *preemptively* writes a rescue
  note as a side effect. It never blocks and, when nothing is relevant, prints
  nothing.

**Why no conflict:** both are *additive* on UserPromptSubmit — CG adds reference
body, ctxrot adds rot/band advice. Claude Code runs each hook independently and
concatenates the additional context; neither mutates the prompt or the other's
output. CG's injection is content-deduped via its own ledger (`was_injected`), and
ctxrot caps its own injection (`guard_inject_max_chars`), so neither becomes a rot
source. They run in either order with the same result.

### SessionStart — additive restore, and **anti-correlated on `source`**

- **CG** (`src/defaults/rehydrator.rs`): recalls the `SNAPSHOT_KEY` item from its
  backing store (written by the PreCompact guard) and re-injects it as
  `additionalContext` so pinned/normative content survives compaction. The module
  doc notes it is *"Most relevant on `source == "compact"`"*. No snapshot → `{}`.
- **ctxrot** (`src/hooks/restore.rs`): injects a compact carryover (Decisions / Open
  todos + a pointer) from the latest rescue/distill note, plus pinned loadset items.
  It **explicitly returns `None` when `input.source == "compact"`** — restore is for
  a fresh session picking up prior work, not the compaction handoff.

**Why no conflict:** additive — CG rebuilds *its own snapshot* of resident norms,
ctxrot surfaces *its own carryover note*; the two read different stores and inject
side by side. They are also neatly anti-correlated on the trigger: on
`source == "compact"` CG's rehydrator is *most* active while ctxrot's restore
deliberately stays silent, so the post-compact handoff is owned by CG; on
`startup`/`resume`/`clear` ctxrot's carryover leads. ctxrot's hooks.json matcher
(`startup|resume|clear`) does not even spawn it on `compact`, and its handler also
returns `None` there, so there is no double injection on `compact`. On
`startup`/`resume`/`clear` both can fire; the injections are independent text blocks.

### PreCompact — both are **side effects**, and **neither blocks**

- **CG** (`src/defaults/guard.rs`): snapshots the transcript into its backing store
  under `SNAPSHOT_KEY` (so SessionStart can rehydrate it) and records a ledger row,
  then returns `CompactDecision::Proceed` **unconditionally**. The doc reserves
  `Block` for the future case where a snapshot genuinely could not be secured; the
  default guard never returns it. `snapshot_transcript` is fail-soft on an
  empty/missing transcript.
- **ctxrot** (`src/hooks/rescue.rs`): streams the recent transcript and writes a
  durable markdown **rescue note** (decisions/todos/files/links), reporting the path
  to **stderr only** — *"PreCompact does not inject context."* It then
  fire-and-forgets a detached async distill. It exits 0; it never blocks compaction.

**Why no conflict:** both are pure pre-compaction side effects that snapshot the
*same source* (the transcript) into *different sinks* — CG into its `SNAPSHOT_KEY`
backing-store entry, ctxrot into a markdown note on disk. Neither blocks (CG always
`Proceed`s; ctxrot only writes a note and exits 0), so compaction proceeds regardless
of order, and each side's snapshot is independently recoverable afterward (CG via the
SessionStart rehydrator, which is routed on `compact` by CG's hooks.json matcher
`startup|resume|clear|compact`; ctxrot's rescue note is read back by its SessionStart
restore on a later `startup`/`resume`/`clear`, NOT on `compact` — restore returns `None`
there and ctxrot's own matcher excludes `compact`).

## Non-overlapping events (for completeness)

- **PreToolUse** — ctxrot only (`preguard`): the *only* hook across both plugins that
  can `deny` a tool call (an unbounded `Read` of a >`gate_file_bytes` file, or a
  `load_deny` glob match). CG registers no PreToolUse handler, so there is nothing to
  conflict with.
- **Stop / SubagentStop** — CG only (`checkpointer`): side-effect-only
  externalization of completed work, output discarded, never blocks. ctxrot registers
  no Stop handler.

## Invariant the integration test pins

CG's contract on every shared event is: **emit a valid envelope and exit 0** (the
sole exception being a `PreCompact` *Block*, which the default `CompactionGuard`
never produces). The integration test
`tests/context_governor_coexistence_with_ctxrot.rs` drives the compiled
`context-governor` binary with representative PostToolUse / UserPromptSubmit /
SessionStart / PreCompact payloads and asserts exactly that — with no `ctxrot` binary
present and no `ctxrot` state on disk — so CG's non-interfering behavior is locked in
independently of ctxrot.
