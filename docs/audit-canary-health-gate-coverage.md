# Audit: canary stage loop health-gate coverage in `scripts/rollout-plugins.sh`

**Type**: read-only audit. No file under `scripts/` or `crates/` was modified to produce this
report. Only this document was created.

**Measurement point**: `git rev-parse --short HEAD`

```
b7f9178b
```

(worktree: `/Users/yuki/.condukt/worktrees/run-20260729-061440-32616-audit-canary-gate-coverage`,
branch `condukt/run-20260729-061440-32616/audit-canary-gate`)

> **Historical snapshot (citation maintenance only, added after the fix).** Everything below
> describes `scripts/rollout-plugins.sh` **as it stood at b7f9178b**, and every FILE:LINE in this
> report refers to that revision. The defect documented here — the health gate running only
> *between* stages, so the last (or only) stage was applied and shipped without ever being
> checked — **has since been fixed**: the gate now runs for every stage, including the only stage
> of a single-stage run, and the guard quoted throughout this report no longer exists. The fix
> lives in `scripts/rollout-plugins.sh` (`run_canary`) with its regression test in
> `scripts/tests/canary-final-stage-gate.sh`. The citations here are therefore marked as a
> historical snapshot rather than renumbered: renumbering them onto post-fix lines would make this
> document assert things about code it never audited. No finding or conclusion below has been
> altered.

---

## 1. Every path in `run_canary` where a stage is applied, and the exact condition under which the health gate runs

`run_canary()` walks stages in a single loop:

<!-- doc-claim-exempt: pre-fix historical snapshot: line numbers are as of measurement point b7f9178b, before the fix that gates every stage; some quoted code no longer exists post-fix -->
`scripts/rollout-plugins.sh:713`
```bash
  for (( s=0; s<nstages; s++ )); do
```

Inside each iteration, a stage is **applied** (copied to its cache version dir, then
registry-repointed) unconditionally — this happens for every stage, including the last one:

(The FILE:LINE column of the table below is a b7f9178b snapshot like every other citation in this
report — see the note at the top; those lines have all shifted since the fix. The per-line
`doc-claim-exempt` markers used elsewhere in this report are deliberately **omitted inside the
table**: such a marker can only sit on the line before the citation, and a marker line between
table rows — or trailing text on the delimiter row — breaks GFM table rendering, backlog
`622e33ca`. This paragraph carries the same statement for the four rows instead.)

| Step | FILE:LINE | Verbatim code |
|---|---|---|
| Copy each plugin in the stage | `scripts/rollout-plugins.sh:761` | `      canary_copy_row "$pn_row"` |
| Collect registry args if the row needs a registry update | `scripts/rollout-plugins.sh:763-765` | ```      if [ "$needs_registry" = "1" ] || [ "$force" = 1 ]; then\n        stage_reg_args+=("$name" "$version" "$target")\n      fi``` |
| Batch-commit the stage's registry pointer (dry-run variant) | `scripts/rollout-plugins.sh:767-769` | ```    if [ "${#stage_reg_args[@]}" -gt 0 ]; then\n      if [ "$dry" = 1 ]; then\n        registry_patch "$REGISTRY" "$OWNER" "$GIT_SHA" --dry-run "${stage_reg_args[@]}" | sed 's/^/  /'``` |
| Batch-commit the stage's registry pointer (real, non-dry-run) | `scripts/rollout-plugins.sh:770-771` | ```      else\n        registry_patch "$REGISTRY" "$OWNER" "$GIT_SHA" "${stage_reg_args[@]}" | sed 's/^/  /'``` |

So "apply a stage" = `canary_copy_row` (plain-copies the plugin dir into its cache version dir) +
`registry_patch` (repoints `installed_plugins.json` at that new version dir) — this happens on
**every** loop iteration, with **no condition** gating it.

The health gate that is supposed to run *between* stages is gated by exactly one condition, and
the entire gate block — the violation-rate check, the rollback-on-spike branch, and the
fail-closed-on-eval-error branch — sits **inside** that one `if`:

<!-- doc-claim-exempt: pre-fix historical snapshot: line numbers are as of measurement point b7f9178b, before the fix that gates every stage; some quoted code no longer exists post-fix -->
`scripts/rollout-plugins.sh:757-758`
```bash
    # Health gate BETWEEN stages (skip the check after the final stage).
    if [ "$s" -lt "$((nstages - 1))" ]; then
```

The block opened by this `if` does not close until:

<!-- doc-claim-exempt: pre-fix historical snapshot: line numbers are as of measurement point b7f9178b, before the fix that gates every stage; some quoted code no longer exists post-fix -->
`scripts/rollout-plugins.sh:850-851`
```bash
      fi
    fi
```

<!-- doc-claim-exempt: pre-fix historical snapshot: line numbers are as of measurement point b7f9178b, before the fix that gates every stage; some quoted code no longer exists post-fix -->
(line 850 closes the inner `if [ "$dry" = 1 ]` branch introduced at `scripts/rollout-plugins.sh:760`;
line 851 closes the outer `if [ "$s" -lt "$((nstages - 1))" ]` opened at line 758). Everything
between lines 759 and 849 — the `echo "  health-gate: checking violation rate (threshold=$canary_threshold)..."`
announcement, the dry-run stub gate call, the real `"$ow" canary-gate ...` invocation, the
`gate_rc` branching, `execute_stage_rollback`, and both `exit 4` / `exit 5` halts — is **only
reached when `s < nstages - 1`**. `grep -n canary-gate scripts/rollout-plugins.sh` returns six <!-- doc-claim-exempt: pre-fix historical snapshot: line numbers are as of measurement point b7f9178b, before the fix that gates every stage; some quoted code no longer exists post-fix -->
lines: four are doc/code comments (`scripts/rollout-plugins.sh:44`, `:122`, `:788`, `:821`) and
**two are executable call sites**, both inside this same `if [ "$s" -lt "$((nstages - 1))" ]`
guard opened at :758 — the dry-run branch's `"$ow" canary-gate --observed-violations 0 --threshold <!-- doc-claim-exempt: pre-fix historical snapshot: line numbers are as of measurement point b7f9178b, before the fix that gates every stage; some quoted code no longer exists post-fix -->
"$canary_threshold" | sed 's/^/  /' || true` at `scripts/rollout-plugins.sh:763`, and the
real-path `gate_args=(canary-gate --threshold "$canary_threshold" ...)` array literal at <!-- doc-claim-exempt: pre-fix historical snapshot: line numbers are as of measurement point b7f9178b, before the fix that gates every stage; some quoted code no longer exists post-fix -->
`scripts/rollout-plugins.sh:802`, invoked two lines later via `"$ow" "${gate_args[@]}"` at <!-- doc-claim-exempt: pre-fix historical snapshot: line numbers are as of measurement point b7f9178b, before the fix that gates every stage; some quoted code no longer exists post-fix -->
`scripts/rollout-plugins.sh:806` (that invocation line does not itself contain the literal string
`canary-gate`, so it does not show up in the grep output, but it is the same call whose arguments
are constructed at :802). The dry-run call (:763) and the real-path call (:802/:806) are mutually
exclusive branches of the inner `if [ "$dry" = 1 ]` (:760), so this correction does not change the
report's substantive conclusion: **every** `canary-gate` invocation — dry-run or real — sits
strictly inside the `s < nstages - 1` guard and is therefore skipped for the last (or only) stage.

**Established by quotation**: the condition guarding the entire health-gate block is <!-- doc-claim-exempt: pre-fix historical snapshot: line numbers are as of measurement point b7f9178b, before the fix that gates every stage; some quoted code no longer exists post-fix -->
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

Output excerpt (captured directly from the tool invocation, not redirected to a file per the
no-`>`/no-`2>&1` constraint; the `...` markers below are elisions in the pasted excerpt, not part
of the tool's actual output):

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
all** — the gate's own `echo "  health-gate: checking violation rate (threshold=$canary_threshold)..."` <!-- doc-claim-exempt: pre-fix historical snapshot: line numbers are as of measurement point b7f9178b, before the fix that gates every stage; some quoted code no longer exists post-fix -->
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

### Anti-vacuity control: a two-plugin run that produces `nstages = 2`

A count of 0 only demonstrates the coverage gap if the `grep -c "health-gate:"` pattern is a real
discriminator — i.e. it must return a non-zero count on *some* run, or the 0 above could just as
well mean the pattern never matches anything (a vacuous non-match), rather than a genuine absence
in this specific single-stage run. To rule that out, I ran the identical command against a
two-plugin invocation, which `overwatch canary-plan` splits into two stages (`--stage-size`
defaults to 1 plugin per stage), so `nstages = 2` and the guard `s < nstages - 1` is true for
`s = 0`:

```
bash scripts/rollout-plugins.sh --plugin schemaguard --plugin autoflow --canary --dry-run | grep -c "health-gate:"
```

Output (re-run directly by me in this worktree, not copied from the verifier's report):

```
1
```

**Measured count: 1**, versus **0** for the single-plugin (`nstages = 1`) run above. This is a
genuine negative control: the same `grep -c "health-gate:"` pattern, against the same script, in
the same worktree, returns a non-zero count once `nstages > 1` puts at least one stage on the
`s < nstages - 1` side of the guard (here, only stage 0 of the two — stage 1, the last stage, still
gets no `health-gate:` line, consistent with the section 1 trace). This confirms the pattern is a
real discriminator and that the single-plugin run's count of 0 is a genuine absence of the
health-gate check, not a vacuous non-match of a broken grep pattern.

---

## 3. Path by which a stage applied without passing a gate still reaches the live machine

Trace of `run_canary`'s tail, after the stage loop exits (whether via the gate never running at
all, as in the `nstages=1` case, or via all stages passing their gates):

<!-- doc-claim-exempt: pre-fix historical snapshot: line numbers are as of measurement point b7f9178b, before the fix that gates every stage; some quoted code no longer exists post-fix -->
`scripts/rollout-plugins.sh:852-855`
```bash
  done

  echo
  echo "canary: all $nstages stage(s) completed."
```

Immediately below, the same function that just finished the stage loop builds a sync list and
calls the shared rebuild/sync function:

<!-- doc-claim-exempt: pre-fix historical snapshot: line numbers are as of measurement point b7f9178b, before the fix that gates every stage; some quoted code no longer exists post-fix -->
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

<!-- doc-claim-exempt: pre-fix historical snapshot: line numbers are as of measurement point b7f9178b, before the fix that gates every stage; some quoted code no longer exists post-fix -->
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
this rollout. For a `GATE_CRATES` member (e.g. `blastguard`, `overwatch` itself — see <!-- doc-claim-exempt: pre-fix historical snapshot: line numbers are as of measurement point b7f9178b, before the fix that gates every stage; some quoted code no longer exists post-fix -->
`scripts/rollout-plugins.sh:128`), this means a fleet-defense gate can be pushed live via
`--canary` with the canary machinery having exercised nothing beyond stage-planning and
rollback-plan *display* (line 702-703, which is explicitly "data only — not executed").

---

## 4. Already fail-closed branches (quoted, not to be weakened)

Two branches inside the health-gate block (which itself only runs when `s < nstages - 1`, per
section 1) are already fail-closed and must **not** be weakened by any future change:

**(a) `gate_rc` neither 0 nor 3 → `execute_stage_rollback` + `exit 5`** (gate could not evaluate):

<!-- doc-claim-exempt: pre-fix historical snapshot: line numbers are as of measurement point b7f9178b, before the fix that gates every stage; some quoted code no longer exists post-fix -->
`scripts/rollout-plugins.sh:829-842`
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

<!-- doc-claim-exempt: pre-fix historical snapshot: line numbers are as of measurement point b7f9178b, before the fix that gates every stage; some quoted code no longer exists post-fix -->
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

<!-- doc-claim-exempt: pre-fix historical snapshot: line numbers are as of measurement point b7f9178b, before the fix that gates every stage; some quoted code no longer exists post-fix -->
**`scripts/rollout-plugins.sh:798-801`** (comment, inside the `gate_rc` eval-error discussion,
discussing the overwatch self-upgrade bootstrap-skew from backlog `2a953ab5`):

```
        # must not silently proceed. Remedies for this known-benign case: roll
        # overwatch out single-stage FIRST (a single-stage canary has no
        # inter-stage gate check, so the skew never arises), or set
        # ROLLOUT_GATE_EVAL_FAILSOFT=1 to explicitly acknowledge and proceed.
```

This is the exact line matching the pattern "a single-stage canary has no inter-stage gate check": <!-- doc-claim-exempt: pre-fix historical snapshot: line numbers are as of measurement point b7f9178b, before the fix that gates every stage; some quoted code no longer exists post-fix -->
`scripts/rollout-plugins.sh:799-800` — `# overwatch out single-stage FIRST (a single-stage canary has no` /
`# inter-stage gate check, so the skew never arises), or set`.

A second, weaker echo of the same premise is emitted at runtime (not a comment, but user-facing
guidance built on the identical assumption, so it will also go stale once the gate always runs):

<!-- doc-claim-exempt: pre-fix historical snapshot: line numbers are as of measurement point b7f9178b, before the fix that gates every stage; some quoted code no longer exists post-fix -->
**`scripts/rollout-plugins.sh:835`**:
```bash
            echo "  (Known benign cause: overwatch self-upgrade bootstrap-skew, rc=2 against the pre-swap binary. Roll out overwatch single-stage first, or set ROLLOUT_GATE_EVAL_FAILSOFT=1 to explicitly proceed.)" >&2
```

`grep -n single-stage scripts/rollout-plugins.sh` (run in the worktree) confirms these are the
**only two** occurrences of the literal string "single-stage" in the file:

```
799:        # overwatch out single-stage FIRST (a single-stage canary has no
835:            echo "  (Known benign cause: overwatch self-upgrade bootstrap-skew, rc=2 against the pre-swap binary. Roll out overwatch single-stage first, or set ROLLOUT_GATE_EVAL_FAILSOFT=1 to explicitly proceed.)" >&2
```

That grep is necessarily incomplete as an inventory of stale prose, because it only catches
occurrences of the literal phrase "single-stage". A third comment states the same soon-to-be-false
premise in different words, without using that phrase, and was found by direct reading rather than
by the grep above:

<!-- doc-claim-exempt: pre-fix historical snapshot: line numbers are as of measurement point b7f9178b, before the fix that gates every stage; some quoted code no longer exists post-fix -->
**`scripts/rollout-plugins.sh:757`** (the comment directly above the guard itself):

```
    # Health gate BETWEEN stages (skip the check after the final stage).
```

This asserts, as current behavior, that the check is skipped "after the final stage" — the exact
premise this report's sections 1-3 show also holds for the *only* stage of a single-stage run.
Once a fix makes the gate always run, this comment becomes false in the same way as the two
identified by the "single-stage" grep, even though it does not itself contain that string.

### CONSTRAINT FOR THE NEXT TASK

<!-- doc-claim-exempt: pre-fix historical snapshot: line numbers are as of measurement point b7f9178b, before the fix that gates every stage; some quoted code no longer exists post-fix -->
The comment at `scripts/rollout-plugins.sh:798-801`, the operator-facing message at <!-- doc-claim-exempt: pre-fix historical snapshot: line numbers are as of measurement point b7f9178b, before the fix that gates every stage; some quoted code no longer exists post-fix -->
`scripts/rollout-plugins.sh:835`, and the guard comment at `scripts/rollout-plugins.sh:757` all
state or imply, as a documented-benign or current-behavior fact, that the health gate is skipped
for the last (or only) stage — the first two frame it as "a single-stage canary has no inter-stage
gate check" and recommend rolling `overwatch` out single-stage as the remedy for the self-upgrade
bootstrap-skew (backlog `2a953ab5`); the third simply documents the skip-on-final-stage behavior
directly. **Once a fix makes the health gate always run — including for the last/only stage — all
three premises become false**, and the recommended remedy ("roll overwatch out single-stage
first") would no longer avoid the bootstrap-skew (it would now trigger the gate-eval-error
fail-closed path unconditionally on such a run, or would need the gate itself to be made
bootstrap-skew-aware). Any change that closes the coverage gap identified in sections 1-3 of this
report **must, in the same commit**, correct or remove:

<!-- doc-claim-exempt: pre-fix historical snapshot: line numbers are as of measurement point b7f9178b, before the fix that gates every stage; some quoted code no longer exists post-fix -->
- the comment at `scripts/rollout-plugins.sh:798-801` (the "remedies for this known-benign case"
  text), <!-- doc-claim-exempt: pre-fix historical snapshot: line numbers are as of measurement point b7f9178b, before the fix that gates every stage; some quoted code no longer exists post-fix -->
- the runtime message at `scripts/rollout-plugins.sh:835` ("Roll out overwatch single-stage
  first, ..."), and <!-- doc-claim-exempt: pre-fix historical snapshot: line numbers are as of measurement point b7f9178b, before the fix that gates every stage; some quoted code no longer exists post-fix -->
- the guard comment at `scripts/rollout-plugins.sh:757` ("Health gate BETWEEN stages (skip the
  check after the final stage)."),

so that none continues to advertise a remedy or describe a behavior the fixed code no longer has.
Leaving any of the three uncorrected after the fix would be a docstring/comment that claims a
behavior the code does not have — a case CLAUDE.md §4 explicitly names as prohibited
("docstring / コメントで実挙動と違う「安全な話」を書く"). Anyone following the list literally
without this third item would leave `:757` stale, which is exactly the hazard this section exists
to prevent.

---

## Summary of findings

<!-- doc-claim-exempt: pre-fix historical snapshot: line numbers are as of measurement point b7f9178b, before the fix that gates every stage; some quoted code no longer exists post-fix -->
- The health-gate block (`scripts/rollout-plugins.sh:759-849`) is entirely subordinate to a single <!-- doc-claim-exempt: pre-fix historical snapshot: line numbers are as of measurement point b7f9178b, before the fix that gates every stage; some quoted code no longer exists post-fix -->
  guard, `if [ "$s" -lt "$((nstages - 1))" ]` (`scripts/rollout-plugins.sh:758`), while stage
  *application* (copy + registry patch, lines 740-755) is unconditional per iteration.
- Measured directly: `bash scripts/rollout-plugins.sh --plugin schemaguard --canary --dry-run |
  grep -c "health-gate:"` → **0**, for a run that produced `nstages = 1`.
- The unconditional `run_rebuild_and_sync` call at the end of `run_canary` <!-- doc-claim-exempt: pre-fix historical snapshot: line numbers are as of measurement point b7f9178b, before the fix that gates every stage; some quoted code no longer exists post-fix -->
  (`scripts/rollout-plugins.sh:870`) means an applied-but-never-gated stage's binary and registry
  pointer become fully live on the running harness with no health verification ever performed. <!-- doc-claim-exempt: pre-fix historical snapshot: line numbers are as of measurement point b7f9178b, before the fix that gates every stage; some quoted code no longer exists post-fix -->
- The two existing rollback branches (`scripts/rollout-plugins.sh:829-842` and <!-- doc-claim-exempt: pre-fix historical snapshot: line numbers are as of measurement point b7f9178b, before the fix that gates every stage; some quoted code no longer exists post-fix -->
  `scripts/rollout-plugins.sh:843-848`) are correctly fail-closed and are not implicated in the
  gap — they are simply unreachable on the last (or only) stage. <!-- doc-claim-exempt: pre-fix historical snapshot: line numbers are as of measurement point b7f9178b, before the fix that gates every stage; some quoted code no longer exists post-fix -->
- Three pieces of prose (`scripts/rollout-plugins.sh:798-801` comment, `scripts/rollout-plugins.sh:835` <!-- doc-claim-exempt: pre-fix historical snapshot: line numbers are as of measurement point b7f9178b, before the fix that gates every stage; some quoted code no longer exists post-fix -->
  runtime message, and `scripts/rollout-plugins.sh:757` guard comment) currently rely on or
  describe the coverage gap as known-benign/current behavior and will need correcting in the same
  commit as any fix that removes the gap.
- Anti-vacuity control: `grep -c "health-gate:"` on a two-plugin (`nstages = 2`) canary dry-run
  returns **1**, versus **0** for the single-plugin (`nstages = 1`) run — confirming the pattern is
  a real discriminator and the 0 above is a genuine absence, not a vacuous non-match.
