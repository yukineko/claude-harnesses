#!/usr/bin/env python3
"""Report workflows that are RED on the shared branch — and how long they have been.

Why this exists
---------------
A `clippy` lint that had been latent since June 20 surfaced only when GitHub's
`stable` toolchain moved under `dtolnay/rust-toolchain@stable`. The resulting
failure was correct and useful. What was broken was that it stayed red for
**89 consecutive runs** and nobody noticed: this repo has no required status
checks, so a red workflow blocks nothing and announces nothing.

The tempting fix — pin the toolchain — trades the detection away to buy the
silence. That is backwards. A pinned lint set cannot find the *next* latent
defect, so the green it produces means "we stopped looking", not "nothing is
wrong". The defect to fix is the silence, not the floating toolchain.

Design rules (both are the repo owner's explicit instructions)
-------------------------------------------------------------
1. **A gate that fails open is worse than no gate.** Every "could not tell"
   path is loud and non-zero. There is exactly one way to reach exit 0: every
   active workflow was fetched and every one of them was judged.
2. **Audit to find problems, not to reach agreement.** Anything that *might* be
   a problem is reported. In particular a workflow whose history is too short
   to bound its streak is NOT demoted to the benign "recently red" class — it
   is reported as undetermined, because the rarely-run workflow is exactly the
   one most likely to have rotted unnoticed.

Rule 2 is why this file fetches per workflow. An earlier version issued one
flat `gh run list --limit 100` and split the result by name. With 15+ active
workflows sharing that window each got 1-10 runs, so a workflow that had failed
its last 30 runs reported a streak of 1 and was printed under "only recently
red". The tool structurally could not observe the 89-run incident it cites
above.

Why pre-push and not CI
-----------------------
This reads GitHub Actions state *about* CI, so running it inside CI is circular
(and on a runner it would mostly report on itself). It also needs an
authenticated `gh`, which a runner does not have by default. Same reasoning as
`check-plugin-rollout.py`: a check whose inputs are machine-local belongs in
`.githooks/pre-push`, where it can actually reach them.

Exit codes are the contract with `.githooks/pre-push`; the hook branches on
them and never parses this output. An earlier version had the hook match the
literal string "check-ci-red: skipped", which meant rewording one message would
silently turn the check into a no-op — the very failure mode it exists to
prevent.

    RC_OK           0  every active workflow fetched and judged; none chronic
    RC_CHRONIC      1  at least one workflow is chronically red
    RC_UNDETERMINED 3  something could not be determined (no gh, no auth,
                       offline, timeout, partial fetch, too-short history,
                       unexpected payload, or an unhandled crash). NEVER means
                       "CI is fine".

argparse's own usage error (2) is deliberately left distinct and is treated by
the hook as undetermined, like any other unrecognized code.
"""

import argparse
import concurrent.futures
import json
import pathlib
import shutil
import subprocess
import sys
import time
from datetime import datetime, timezone

RC_OK = 0
RC_CHRONIC = 1
RC_UNDETERMINED = 3

DEFAULT_BRANCH = "main"
DEFAULT_CHRONIC = 3
# Runs fetched PER WORKFLOW. Must exceed --chronic by enough that a streak can
# usually be bounded rather than truncated; a truncated streak is undetermined,
# not free evidence.
DEFAULT_DEPTH = 25
GH_TIMEOUT_SECS = 25
# Whole-check wall budget. A pre-push hook that hangs is its own outage, so the
# budget is real — but exhausting it makes the unfetched workflows undetermined
# and LOUD, never silently skipped.
TOTAL_BUDGET_SECS = 45
MAX_PARALLEL = 6

# Conclusions that neither break a streak nor count toward one. A cancelled or
# skipped run carries no verdict about the code: treating it as green would
# hide a streak that spans it, and treating it as red would invent one.
NEUTRAL = {"cancelled", "skipped", "neutral", "stale", "action_required", None}
RED = {"failure", "timed_out", "startup_failure"}


class Unavailable(Exception):
    """A determination could not be made. Never a verdict about CI health."""


def _gh_json(args, timeout=GH_TIMEOUT_SECS):
    """Run a `gh ... --json ...` command and return the parsed list."""
    if shutil.which("gh") is None:
        raise Unavailable("gh CLI not installed")
    try:
        p = subprocess.run(["gh"] + args, capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        raise Unavailable(f"gh timed out after {timeout}s ({' '.join(args[:2])})")
    except OSError as e:
        raise Unavailable(f"could not run gh ({e})")
    if p.returncode != 0:
        lines = (p.stderr or p.stdout or "").strip().splitlines()
        raise Unavailable(f"gh failed: {lines[0] if lines else 'no output'}")
    try:
        data = json.loads(p.stdout or "[]")
    except json.JSONDecodeError as e:
        raise Unavailable(f"gh returned unparseable JSON ({e})")
    if not isinstance(data, list):
        raise Unavailable("gh returned an unexpected JSON shape (expected a list)")
    return data


def is_manual_only(path):
    """Tri-state: True (dispatch-only), False (has a real trigger), None (could
    not tell).

    A dispatch-only workflow is not expected to have any branch history, so
    "never ran on main" is its designed state, not evidence of rot. Without this,
    it sits in UNDETERMINED forever and makes the undetermined signal permanent —
    which is how an alarm becomes scenery, the exact decay this checker exists to
    prevent.

    NONE IS NOT FALSE, and collapsing the two was a defect this docstring used to
    describe away: the text claimed "anything it cannot read stays undetermined"
    while every failure path returned False, which the caller reads as a positive
    finding ("this workflow has a real trigger, judge its history"). A missing
    PyYAML made that the answer for EVERY workflow at once, silently — the CI
    image has no PyYAML, so the whole exclusion feature was dead there and only
    the unit tests noticed.

    Returning None instead pushes the workflow into the undetermined bucket,
    where the reader is told the trigger could not be read and why. This is
    deliberately narrow: it reads the trigger DECLARATION rather than guessing
    from an absence of runs, so it recognises a known-by-design state rather than
    suppressing an unknown one. Excluded workflows are still named in `report`,
    so the exclusion itself stays visible and reviewable, never silent.
    """
    if not path:
        return None
    try:
        import yaml
    except ImportError:
        return None
    try:
        doc = yaml.safe_load(pathlib.Path(path).read_text(encoding="utf-8"))
    except (OSError, yaml.YAMLError):
        return None
    if not isinstance(doc, dict):
        return None
    # PyYAML resolves the bare key `on` to the boolean True (YAML 1.1), so the
    # trigger block can arrive under either key depending on how it was quoted.
    # `.get("on", .get(True))` mis-handles an explicit `on: null`; check
    # membership so a present-but-empty trigger is undetermined, not False.
    if "on" in doc:
        trig = doc["on"]
    elif True in doc:
        trig = doc[True]
    else:
        return None
    if isinstance(trig, str):
        return trig == "workflow_dispatch"
    if isinstance(trig, (list, dict)):
        return set(trig) == {"workflow_dispatch"}
    return None


def manual_only_unavailable_reason():
    """Why `is_manual_only` cannot answer, or None when it can.

    Kept separate so `report` can tell the reader WHICH capability is missing
    instead of printing an unexplained pile of undetermined workflows.
    """
    try:
        import yaml  # noqa: F401
    except ImportError:
        return ("PyYAML is not installed, so no workflow's trigger declaration "
                "could be read (pip install pyyaml)")
    return None


def list_workflows():
    """Active workflows, split into (judgeable, manual_only, unreadable).

    A disabled workflow is excluded outright: it is not expected to run at all.
    A manual-only workflow is separated rather than dropped, so `report` can name
    it as intentionally not judged.

    The third bucket is the one that must not be folded into either of the other
    two. A workflow whose trigger could not be READ is not known to be
    dispatch-only (so excluding it would hide a genuinely rotting workflow) and
    is not known to have a real trigger either (so judging it can invent a
    chronic-red finding about a workflow that was never meant to run on main).
    Both collapses are wrong in a different direction, which is precisely why it
    gets its own bucket and its own line in the report.
    """
    data = _gh_json(["workflow", "list", "--limit", "100", "--json", "id,name,path,state"])
    judgeable, manual, unreadable = [], [], []
    for w in data:
        if not isinstance(w, dict):
            raise Unavailable("gh workflow list returned a non-object entry")
        if w.get("state") != "active":
            continue
        wid, name = w.get("id"), w.get("name")
        if wid is None or not name:
            raise Unavailable("gh workflow list returned an entry without id/name")
        entry = {"id": wid, "name": name, "path": w.get("path") or ""}
        verdict = is_manual_only(entry["path"])
        if verdict is None:
            unreadable.append(entry)
        elif verdict:
            manual.append(entry)
        else:
            judgeable.append(entry)
    return judgeable, manual, unreadable


def fetch_runs(workflow_id, branch, depth):
    data = _gh_json([
        "run", "list",
        "--workflow", str(workflow_id),
        "--branch", branch,
        "--limit", str(depth),
        "--json", "conclusion,status,createdAt,url",
    ])
    for r in data:
        if not isinstance(r, dict):
            raise Unavailable("gh run list returned a non-object entry")
    return data


def red_streak(runs):
    """Return (streak, oldest_index, exhausted).

    `runs` must be newest-first (the order `gh run list` returns).

    - streak: number of consecutive RED conclusions from the newest end,
      skipping NEUTRAL ones (they carry no verdict).
    - oldest_index: index into `runs` of the oldest run counted in the streak,
      or None when streak == 0. Returned rather than recomputed as
      `runs[streak - 1]`, which was wrong whenever a neutral sat inside the
      streak: the count skips neutrals but the index does not, so every neutral
      shifted the reported start date one slot too recent.
    - exhausted: True when the scan reached the end of `runs` without finding a
      non-red verdict, i.e. the streak is a LOWER BOUND and may predate the
      fetched window.
    """
    streak = 0
    oldest = None
    for i, r in enumerate(runs):
        c = r.get("conclusion")
        if r.get("status") not in ("completed", None) or c in NEUTRAL:
            continue
        if c in RED:
            streak += 1
            oldest = i
        else:
            return streak, oldest, False
    return streak, oldest, True


def classify(name, runs, chronic):
    """Classify one workflow's history into exactly one bucket.

    Returns (bucket, row) where bucket is "green", "chronic", "fresh" or
    "undetermined".

    The undetermined bucket is the load-bearing one. A red streak that consumes
    every fetched run is a lower bound: if that bound is already >= chronic the
    verdict is safe (it is chronic, possibly worse), but if it is below the
    threshold we genuinely cannot tell whether this workflow is fine or has
    been failing for a year. Calling that "recently red" — the benign class —
    is what let a rarely-run workflow rot unseen. It is a problem until proven
    otherwise.
    """
    if not runs:
        # An active workflow with no history on this branch cannot be judged.
        # Reporting it as green would be the exact demotion this bucket exists
        # to prevent.
        return "undetermined", {
            "workflow": name,
            "why": "active, but has never run on this branch",
        }

    streak, oldest_idx, exhausted = red_streak(runs)
    if streak == 0:
        return "green", None

    oldest = runs[oldest_idx]
    row = {
        "workflow": name,
        "streak": streak,
        "truncated": exhausted,
        "since": (oldest.get("createdAt") or "")[:10],
        "days": days_since(oldest.get("createdAt")),
        "url": runs[0].get("url") or "",
    }
    if streak >= chronic:
        return "chronic", row
    if exhausted:
        row["why"] = (
            f"all {streak} fetched run(s) are failures, so the streak is a lower "
            f"bound — cannot tell whether it exceeds {chronic}"
        )
        return "undetermined", row
    return "fresh", row


def days_since(ts):
    if not ts:
        return None
    try:
        dt = datetime.fromisoformat(ts.replace("Z", "+00:00"))
    except ValueError:
        return None
    return max(0, (datetime.now(timezone.utc) - dt).days)


def describe(row):
    n = f">={row['streak']}" if row.get("truncated") else str(row.get("streak"))
    age = f", {row['days']}d" if row.get("days") is not None else ""
    since = f" since {row['since']}" if row.get("since") else ""
    return f"  {row['workflow']:<32} {n} consecutive failures{since}{age}"


def collect(branch, depth, chronic, budget=TOTAL_BUDGET_SECS, now=time.monotonic):
    """Fetch and classify every active workflow.

    Returns (buckets, undetermined_reasons). A workflow that could not be
    fetched — for any reason, including the budget running out — lands in
    undetermined with its reason. Nothing is ever dropped silently.
    """
    workflows, manual_only, unreadable = list_workflows()
    if not workflows and not manual_only and not unreadable:
        raise Unavailable("no active workflows found")

    deadline = now() + budget
    buckets = {"green": [], "chronic": [], "fresh": [], "undetermined": [],
               "manual_only": [w["name"] for w in manual_only]}

    # A workflow whose trigger could not be read is undetermined BEFORE any run
    # history is considered: we do not know whether it is even supposed to run on
    # this branch, so neither "green" nor "chronic" would mean anything about it.
    # One shared reason is attached when the cause is global (no PyYAML), so the
    # reader gets the missing capability instead of N identical mystery lines.
    global_why = manual_only_unavailable_reason()
    for wf in unreadable:
        why = global_why or f"could not read the trigger declaration in {wf['path'] or '<no path>'}"
        buckets["undetermined"].append({"workflow": wf["name"], "why": why})

    def work(wf):
        if now() >= deadline:
            raise Unavailable("whole-check time budget exhausted before fetch")
        return fetch_runs(wf["id"], branch, depth)

    with concurrent.futures.ThreadPoolExecutor(max_workers=MAX_PARALLEL) as pool:
        futures = {pool.submit(work, wf): wf for wf in workflows}
        for fut, wf in futures.items():
            try:
                runs = fut.result()
            except Unavailable as e:
                buckets["undetermined"].append({"workflow": wf["name"], "why": str(e)})
                continue
            except Exception as e:  # noqa: BLE001 - any crash is undetermined, never green
                buckets["undetermined"].append(
                    {"workflow": wf["name"], "why": f"unexpected error: {e!r}"}
                )
                continue
            bucket, row = classify(wf["name"], runs, chronic)
            if bucket == "green":
                buckets["green"].append(wf["name"])
            else:
                buckets[bucket].append(row)

    for key in ("chronic", "fresh"):
        buckets[key].sort(key=lambda r: -r["streak"])
    buckets["undetermined"].sort(key=lambda r: r["workflow"])
    return buckets


def report(buckets, branch, chronic):
    """Print the report and return the exit code."""
    chronic_rows = buckets["chronic"]
    undetermined = buckets["undetermined"]
    fresh = buckets["fresh"]

    if chronic_rows:
        print(f"CHRONICALLY RED on {branch} "
              f"({len(chronic_rows)} workflow(s) failing {chronic}+ runs in a row):")
        for row in chronic_rows:
            print(describe(row))
            if row.get("url"):
                print(f"      {row['url']}")

    if undetermined:
        if chronic_rows:
            print()
        print(f"UNDETERMINED ({len(undetermined)} workflow(s) could not be judged — "
              f"this is NOT an all-clear):")
        for row in undetermined:
            if row.get("streak"):
                print(describe(row))
                print(f"      {row['why']}")
            else:
                print(f"  {row['workflow']:<32} {row['why']}")

    if fresh:
        if chronic_rows or undetermined:
            print()
        print(f"Recently red, under the {chronic}-run threshold "
              f"(bounded — not a problem yet):")
        for row in fresh:
            print(describe(row))

    # Named, not silently dropped: an exclusion the reader cannot see is
    # indistinguishable from a blind spot.
    manual = buckets.get("manual_only") or []
    if manual:
        if chronic_rows or undetermined or fresh:
            print()
        print(f"Not judged by design ({len(manual)} manual-only workflow(s); "
              f"workflow_dispatch is their only trigger, so they are not expected "
              f"to run on {branch}):")
        for name in sorted(manual):
            print(f"  {name}")

    if chronic_rows or undetermined:
        print("\nThese block nothing — this repo has no required status checks, so a red")
        print("workflow stays red until someone looks. Inspect one with:")
        print(f"    gh run view --log-failed --branch {branch}")
        return RC_CHRONIC if chronic_rows else RC_UNDETERMINED

    n = len(buckets["green"]) + len(fresh)
    print(f"OK: all {n} judgeable workflow(s) on {branch} judged; none chronically red.")
    return RC_OK


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--branch", default=DEFAULT_BRANCH,
                    help=f"branch to inspect (default: {DEFAULT_BRANCH})")
    ap.add_argument("--chronic", type=int, default=DEFAULT_CHRONIC,
                    help=f"streak length that counts as chronic (default: {DEFAULT_CHRONIC})")
    ap.add_argument("--depth", type=int, default=DEFAULT_DEPTH,
                    help=f"runs to fetch per workflow (default: {DEFAULT_DEPTH})")
    ap.add_argument("--budget", type=float, default=TOTAL_BUDGET_SECS,
                    help=f"whole-check wall budget in seconds (default: {TOTAL_BUDGET_SECS})")
    args = ap.parse_args()

    # A nonsense threshold used to print to stderr and return 0, which the hook
    # could not distinguish from an all-clear — a check that never ran, reported
    # as green. Undetermined is the honest code.
    if args.chronic < 1:
        print(f"check-ci-red: --chronic must be >= 1 (got {args.chronic}); "
              f"nothing was checked", file=sys.stderr)
        return RC_UNDETERMINED
    if args.depth < 1:
        print(f"check-ci-red: --depth must be >= 1 (got {args.depth}); "
              f"nothing was checked", file=sys.stderr)
        return RC_UNDETERMINED

    try:
        buckets = collect(args.branch, args.depth, args.chronic, budget=args.budget)
    except Unavailable as e:
        print(f"check-ci-red: could not determine CI health ({e}). "
              f"This is NOT an all-clear.", file=sys.stderr)
        return RC_UNDETERMINED
    except Exception as e:  # noqa: BLE001
        # An uncaught crash used to exit 1 — the same code as "chronically red"
        # — so the hook narrated a traceback as a CI verdict.
        print(f"check-ci-red: crashed ({e!r}). This is NOT an all-clear.", file=sys.stderr)
        return RC_UNDETERMINED

    return report(buckets, args.branch, args.chronic)


if __name__ == "__main__":
    sys.exit(main())
