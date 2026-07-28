# Audit: canary stage loop health-gate coverage in `scripts/rollout-plugins.sh`

**Type**: read-only audit. No file under `scripts/` or `crates/` was modified to produce this
report. Only this document was created.

**Measurement point**: `git rev-parse --short HEAD`

```
b7f9178b
```

(worktree: `/Users/yuki/.condukt/worktrees/run-20260729-061440-32616-audit-canary-gate-coverage`,
branch `condukt/run-20260729-061440-32616/audit-canary-gate`)

---

## 1. Every path in `run_canary` where a stage is applied, and the exact condition under which the health gate runs

`run_canary()` walks stages in a single loop:

`scripts/rollout-plugins.sh:713`
```bash
  for (( s=0; s<nstages; s++ )); do
```

Inside each iteration, a stage is **applied** (copied to its cache version dir, then
registry-repointed) unconditionally — this happens for every stage, including the last one:

| Step | FILE:LINE | Verbatim code |
|---|---|---|
| Copy each plugin in the stage | `scripts/rollout-plugins.sh:743` | `      canary_copy_row "$pn_row"` |
| Collect registry args if the row needs a registry update | `scripts/rollout-plugins.sh:745-748` | ```      if [ "$needs_registry" = "1" ] || [ "$force" = 1 ]; then\n        stage_reg_args+=("$name" "$version" "$target")\n      fi``` |
| Batch-commit the stage's registry pointer (dry-run variant) | `scripts/rollout-plugins.sh:749-751` | ```    if [ "${#stage_reg_args[@]}" -gt 0 ]; then\n      if [ "$dry" = 1 ]; then\n        registry_patch "$REGISTRY" "$OWNER" "$GIT_SHA" --dry-run "${stage_reg_args[@]}" | sed 's/^/  /'``` |
| Batch-commit the stage's registry pointer (real, non-dry-run) | `scripts/rollout-plugins.sh:752-753` | ```      else\n        registry_patch "$REGISTRY" "$OWNER" "$GIT_SHA" "${stage_reg_args[@]}" | sed 's/^/  /'``` |

So "apply a stage" = `canary_copy_row` (plain-copies the plugin dir into its cache version dir) +
`registry_patch` (repoints `installed_plugins.json` at that new version dir) — this happens on
**every** loop iteration, with **no condition** gating it.

The health gate that is supposed to run *between* stages is gated by exactly one condition, and
the entire gate block — the violation-rate check, the rollback-on-spike branch, and the
fail-closed-on-eval-error branch — sits **inside** that one `if`:

`scripts/rollout-plugins.sh:757-758`
```bash
    # Health gate BETWEEN stages (skip the check after the final stage).
    if [ "$s" -lt "$((nstages - 1))" ]; then
```

The block opened by this `if` does not close until:

`scripts/rollout-plugins.sh:850-851`
```bash
      fi
    fi
```

(line 850 closes the inner `if [ "$dry" = 1 ]` branch introduced at `scripts/rollout-plugins.sh:760`;
line 851 closes the outer `if [ "$s" -lt "$((nstages - 1))" ]` opened at line 758). Everything
between lines 759 and 849 — the `echo "  health-gate: checking violation rate..."` announcement,
the dry-run stub gate call, the real `"$ow" canary-gate ...` invocation, the `gate_rc` branching,
`execute_stage_rollback`, and both `exit 4` / `exit 5` halts — is **only reached when
`s < nstages - 1`**. There is no other call site of `canary-gate` inside `run_canary` and no other
place the health gate is invoked in the whole script (confirmed by `grep -n canary-gate
scripts/rollout-plugins.sh`, which returns only the doc-comment mentions and the one call at line
806, all inside this same `if`).

**Established by quotation**: the condition guarding the entire health-gate block is
`[ "$s" -lt "$((nstages - 1))" ]` (`scripts/rollout-plugins.sh:758`), i.e. "stage index is less
than nstages minus 1". The apply step (copy + registry patch, lines 740-755) has no such guard and
runs for every stage unconditionally, including the last (and, when `nstages == 1`, the only)
stage.

---

## 2. Measurement: when `nstages` is 1, the gate executes zero times

Command run verbatim in the worktree:

```
bash scripts/rollout-plugins.sh --plugin schemaguard --canary --dry-run
```

Full output (captured directly from the tool invocation, not redirected to a file per the
no-`>`/no-`2>&1` constraint):

```
repo:        /Users/yuki/.condukt/worktrees/run-20260729-061440-32616-audit-canary-gate-coverage
cache:       /Users/yuki/.claude/plugins/cache/yukineko
registry:    /Users/yuki/.claude/plugins/installed_plugins.json
dry-run:     yes   force: no   no-rebuild: no   no-sync: no
canary:      yes   stage-size: 1   threshold: 2
plugins:     schemaguard

canary: using overwatch binary: /Users/yuki/.claude/plugins/cache/yukineko/overwatch/0.2.12/bin/overwatch
=== canary stage plan ===
{
  "stages": [
    {
      "index": 0,
      "plugins": [
        "schemaguard"
      ]
    }
  ]
}
=== canary rollback plan (data only — not executed) ===
{
  "stage_index": 0,
  "targets": [
    {
      "name": "schemaguard",
      ...
    }
  ]
}

canary: 1 stage(s), stage-size=1, threshold=2

--- stage 0: schemaguard ---

canary: all 1 stage(s) completed.

[dry-run] would run: scripts/rebuild-plugins.sh --no-clean --only=schemaguard (CLAUDE_PLUGIN_CACHE=/Users/yuki/.claude/plugins/cache/yukineko)

sync: no plugin ships scripts/sync-plugin-assets.sh

>>> scripts/prune-plugin-cache.py --dry-run
--- prune: 0 stale dir(s) listed, 46 kept (in use or undetermined), would free 0.0 MB
...
verify: skipped — this run was filtered to: schemaguard
        (run without --plugin to verify and enforce the whole fleet)

done (canary).
```

Note: with `--plugin schemaguard`, `overwatch canary-plan` produced exactly one stage
(`"stages": [{ "index": 0, ... }]`), so `nstages = 1`. The stage-loop body between `--- stage 0:
schemaguard ---` and `canary: all 1 stage(s) completed.` contains **no `health-gate:` line at
all** — the gate's own `echo "  health-gate: checking violation rate..."`
(`scripts/rollout-plugins.sh:759`) never printed.

Count measurement (same command, piped to `grep -c` — no `>`/`2>&1` used):

```
bash scripts/rollout-plugins.sh --plugin schemaguard --canary --dry-run | grep -c "health-gate:"
```

Output:

```
0
```

**Measured count: 0.** This confirms by direct execution — not by reasoning about the code — that
with `nstages = 1` the loop's single iteration has `s = 0` and the guard evaluates
`0 -lt (1 - 1)` = `0 -lt 0` = false, so the entire health-gate block (lines 759-849) is skipped,
exactly as predicted by the code trace in section 1.

---

## 3. Path by which a stage applied without passing a gate still reaches the live machine

Trace of `run_canary`'s tail, after the stage loop exits (whether via the gate never running at
all, as in the `nstages=1` case, or via all stages passing their gates):

`scripts/rollout-plugins.sh:852-855`
```bash
  done

  echo
  echo "canary: all $nstages stage(s) completed."
```

Immediately below, the same function that just finished the stage loop builds a sync list and
calls the shared rebuild/sync function:

`scripts/rollout-plugins.sh:857-870`
```bash
  # All stages completed without a rollback halt (the rollback branch exits 4
  # before reaching here), so finish the rollout exactly like the normal path:
  # swap in freshly built binaries and refresh skills/hooks/agents. Without
  # this the canary path would leave the running harness on stale binaries
  # (finding 4). Build the sync list from the rolled-out plugins that ship a
  # sync script (mirrors the normal path).
  local -a canary_synced=()
  local pn2 sname sver ssrc rest srcdir2
  for pn2 in "${ordered_names[@]}"; do
    IFS=$'\t' read -r sname sver ssrc rest <<<"$(row_for_name "$pn2")"
    srcdir2="$REPO/$ssrc"
    [ -f "$srcdir2/scripts/sync-plugin-assets.sh" ] && canary_synced+=("$sname:$srcdir2")
  done
  run_rebuild_and_sync ${canary_synced[@]+"${canary_synced[@]}"}
```

`run_rebuild_and_sync` (called unconditionally here, gated only by `--no-rebuild`/`--no-sync`/
`--dry-run`, none of which are gate-related) is where the binary actually gets deployed:

`scripts/rollout-plugins.sh:507,511-518`
```bash
run_rebuild_and_sync() {
  ...
  if [ "$no_rebuild" = 1 ]; then
    echo "rebuild: skipped (--no-rebuild)"
  elif [ "$dry" = 1 ]; then
    echo "[dry-run] would run: scripts/rebuild-plugins.sh --no-clean --only=$only_arg (CLAUDE_PLUGIN_CACHE=$CACHE)"
  else
    echo ">>> scripts/rebuild-plugins.sh --no-clean --only=$only_arg"
    CLAUDE_PLUGIN_CACHE="$CACHE" bash "$REPO/scripts/rebuild-plugins.sh" --no-clean --only="$only_arg"
  fi
```

So the full path from "applied" to "live" for a stage that never passed (or never even attempted)
a health gate is:

1. `canary_copy_row` (line 743, inside the per-stage loop, unconditional) plain-copies the plugin
   source into a fresh `<cache>/<name>/<version>/` dir.
2. `registry_patch` (lines 749-753, unconditional) repoints `installed_plugins.json`'s
   `"<name>@yukineko"` entry at that new version dir — this is the point at which the change is
   **committed** to the running harness's registry.
3. The stage loop exits (`done` at line 852) without ever having reached the `if [ "$s" -lt
   "$((nstages - 1))" ]` body when `nstages == 1` (or, for the *last* stage of a multi-stage run,
   by the same guard — the final stage's apply is likewise never gate-checked, since the gate only
   runs *between* stages).
4. `run_canary` unconditionally calls `run_rebuild_and_sync` (line 870), which runs
   `rebuild-plugins.sh` — this is what swaps the freshly built **binary** into the cache dir and
   makes the new code path executable by the live harness, and (unless `--no-sync`) runs the
   plugin's `sync-plugin-assets.sh` to refresh skills/hooks/agents on disk where Claude Code reads
   them.

**What ends up in an unverified-but-committed state**: for a single-stage canary (`nstages == 1`,
which is what `--plugin <one-name> --canary` produces, per the measurement in section 2), the
entire rollout — copy, registry repoint, binary rebuild-and-swap, and asset sync — completes with
**zero invocations of `overwatch canary-gate`**. The plugin's `installed_plugins.json` entry, its
on-disk binary, and its skills/hooks are all live and authoritative for the running harness, yet no
violation-rate check (`Problem-2.1`'s raw-spike/systemic combined verdict) was ever consulted for
this rollout. For a `GATE_CRATES` member (e.g. `blastguard`, `overwatch` itself — see
`scripts/rollout-plugins.sh:128`), this means a fleet-defense gate can be pushed live via
`--canary` with the canary machinery having exercised nothing beyond stage-planning and
rollback-plan *display* (line 702-703, which is explicitly "data only — not executed").

---

## 4. Already fail-closed branches (quoted, not to be weakened)

Two branches inside the health-gate block (which itself only runs when `s < nstages - 1`, per
section 1) are already fail-closed and must **not** be weakened by any future change:

**(a) `gate_rc` neither 0 nor 3 → `execute_stage_rollback` + `exit 5`** (gate could not evaluate):

`scripts/rollout-plugins.sh:829-841`
```bash
        if [ "$gate_rc" -ne 0 ] && [ "$gate_rc" -ne 3 ]; then
          if [ "${ROLLOUT_GATE_EVAL_FAILSOFT:-0}" = 1 ]; then
            echo "  health-gate: eval error (rc=$gate_rc) — ROLLOUT_GATE_EVAL_FAILSOFT=1 set; explicit operator override to PROCEED (fail-soft, acknowledged)" >&2
            gate_rc=0
          else
            echo "  health-gate: CANNOT EVALUATE (rc=$gate_rc) — canary health unverifiable; failing CLOSED, rolling back stage $s and halting without advancing." >&2
            echo "  (Known benign cause: overwatch self-upgrade bootstrap-skew, rc=2 against the pre-swap binary. Roll out overwatch single-stage first, or set ROLLOUT_GATE_EVAL_FAILSOFT=1 to explicitly proceed.)" >&2
            # emit_record=0: there was NO health verdict/violation, so do not
            # write a `raw` violation-rollback event (it would be a false record).
            execute_stage_rollback "$ow" "$s" "$stage_names" 0
            echo "canary: HALTED at stage $s — health gate could not evaluate (fail-closed)." >&2
            exit 5
          fi
        fi
```

**(b) `gate_rc` is 3 (rollback advised) → `execute_stage_rollback` + `exit 4`**:

`scripts/rollout-plugins.sh:843-848`
```bash
        if [ "$gate_rc" -ne 0 ]; then
          echo "  health-gate: ROLLBACK — raw-spike or systemic recurrence detected; rolling back stage $s and halting" >&2
          execute_stage_rollback "$ow" "$s" "$stage_names" 1
          echo "canary: HALTED at stage $s after auto-rollback." >&2
          exit 4
        fi
```

(By the time control reaches line 843, branch (a) has already returned `gate_rc=0` if
`ROLLOUT_GATE_EVAL_FAILSOFT=1` was set, or has exited at line 840 otherwise — so `gate_rc -ne 0` at
line 843 can only be `gate_rc == 3`, matching the doc comment at line 808: "Exit 0 = PROCEED, exit
3 = ROLLBACK advised (raw OR systemic).")

**This report does not propose weakening (a) or (b).** The coverage gap identified in sections
1-3 is that these two fail-closed branches are simply never *reached* for the last stage of any
canary run (including the only stage of a single-stage run) — the branches themselves are sound.

---

## 5. Comments relying on "a single-stage canary has no inter-stage gate check" as a remedy

**`scripts/rollout-plugins.sh:798-801`** (comment, inside the `gate_rc` eval-error discussion,
discussing the overwatch self-upgrade bootstrap-skew from backlog `2a953ab5`):

```
        # must not silently proceed. Remedies for this known-benign case: roll
        # overwatch out single-stage FIRST (a single-stage canary has no
        # inter-stage gate check, so the skew never arises), or set
        # ROLLOUT_GATE_EVAL_FAILSOFT=1 to explicitly acknowledge and proceed.
```

This is the exact line matching the pattern "a single-stage canary has no inter-stage gate check":
`scripts/rollout-plugins.sh:799-800` — `# overwatch out single-stage FIRST (a single-stage canary has no` /
`# inter-stage gate check, so the skew never arises), or set`.

A second, weaker echo of the same premise is emitted at runtime (not a comment, but user-facing
guidance built on the identical assumption, so it will also go stale once the gate always runs):

**`scripts/rollout-plugins.sh:835`**:
```bash
            echo "  (Known benign cause: overwatch self-upgrade bootstrap-skew, rc=2 against the pre-swap binary. Roll out overwatch single-stage first, or set ROLLOUT_GATE_EVAL_FAILSOFT=1 to explicitly proceed.)" >&2
```

`grep -n single-stage scripts/rollout-plugins.sh` (run in the worktree) confirms these are the
**only two** occurrences of "single-stage" in the file:

```
799:        # overwatch out single-stage FIRST (a single-stage canary has no
835:            echo "  (Known benign cause: overwatch self-upgrade bootstrap-skew, rc=2 against the pre-swap binary. Roll out overwatch single-stage first, or set ROLLOUT_GATE_EVAL_FAILSOFT=1 to explicitly proceed.)" >&2
```

### CONSTRAINT FOR THE NEXT TASK

Both the comment at `scripts/rollout-plugins.sh:798-801` and the operator-facing message at
`scripts/rollout-plugins.sh:835` state or imply, as a documented-benign fact, that "a single-stage
canary has no inter-stage gate check" and recommend rolling `overwatch` out single-stage as the
remedy for the self-upgrade bootstrap-skew (backlog `2a953ab5`). **Once a fix makes the health gate
always run — including for the last/only stage — this premise becomes false**, and the recommended
remedy ("roll overwatch out single-stage first") would no longer avoid the bootstrap-skew (it would
now trigger the gate-eval-error fail-closed path unconditionally on such a run, or would need the
gate itself to be made bootstrap-skew-aware). Any change that closes the coverage gap identified in
sections 1-3 of this report **must, in the same commit**, correct or remove:

- the comment at `scripts/rollout-plugins.sh:798-801` (the "remedies for this known-benign case"
  text), and
- the runtime message at `scripts/rollout-plugins.sh:835` ("Roll out overwatch single-stage
  first, ..."),

so that neither continues to advertise a remedy the fixed code no longer provides. Leaving either
uncorrected after the fix would be a docstring/comment that claims a behavior the code does not
have — a case CLAUDE.md §4 explicitly names as prohibited ("docstring / コメントで実挙動と違う
「安全な話」を書く").

---

## Summary of findings

- The health-gate block (`scripts/rollout-plugins.sh:759-849`) is entirely subordinate to a single
  guard, `if [ "$s" -lt "$((nstages - 1))" ]` (`scripts/rollout-plugins.sh:758`), while stage
  *application* (copy + registry patch, lines 740-755) is unconditional per iteration.
- Measured directly: `bash scripts/rollout-plugins.sh --plugin schemaguard --canary --dry-run |
  grep -c "health-gate:"` → **0**, for a run that produced `nstages = 1`.
- The unconditional `run_rebuild_and_sync` call at the end of `run_canary`
  (`scripts/rollout-plugins.sh:870`) means an applied-but-never-gated stage's binary and registry
  pointer become fully live on the running harness with no health verification ever performed.
- The two existing rollback branches (`scripts/rollout-plugins.sh:829-841` and
  `scripts/rollout-plugins.sh:843-848`) are correctly fail-closed and are not implicated in the
  gap — they are simply unreachable on the last (or only) stage.
- Two pieces of prose (`scripts/rollout-plugins.sh:798-801` comment, `scripts/rollout-plugins.sh:835`
  runtime message) currently rely on the coverage gap as a known-benign remedy and will need
  correcting in the same commit as any fix that removes the gap.
