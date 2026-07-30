#!/usr/bin/env python3
"""Periodic freshness audit of this repo's RECORDS (backlog 0f55003a).

WHY THIS EXISTS
    Every gate here checks the code. Nothing checked the records the gates
    write. That matters because the records are the input to the next audit: if
    they rot, the next audit is not merely less useful, it is invalid — it will
    conclude "clean" from a ledger that stopped being written to.

    The four rot modes below are not hypothetical. Each was observed before this
    script existed (measurements dated inline, at the rev named in BASELINE_REV):

      1. `check-doc-claims.py` reports verdict "clean" while 140 `path:line`
         claims in docs/ cite lines or quotes that no longer exist. They are all
         exempt, and exempt is rendered as clean.
      2. `overwatch audit-metrics` has reported `converging: NO` for six
         Continuous-Audit rounds (new findings 5 -> 1 -> 13 -> 0 -> 14 -> 15) and
         nothing consumes that flag.
      3. `overwatch review-metrics` prints its stale-undisposed count and exits
         0 (crates/overwatch/src/disposition_cli.rs). 42 rows sit on the review
         queue against 0 dispositions ever recorded.
      4. `backlog fail` does not retire an item; it defers it two days, after
         which `Task::is_pending` counts it again (crates/backlog/src/task.rs).
         A rejected item resurfaces silently.

    So: measure all four on a schedule, and escalate a threshold breach onto the
    one surface a human already reads (`overwatch review-queue`).

WHERE THE SCHEDULE COMES FROM
    The `daily` plugin, whose SessionStart hook runs registered shell tasks at
    most once per calendar day. That is deliberate and not a fallback: it is
    LOCAL. A CI schedule would put the periodicity — and the visibility of a
    breach — on a service this repo does not control, which CLAUDE.md section 7
    prohibits outright for anything that gates the flow, and which is a bad
    trade even for advisory jobs. `daily` needs no daemon, no clock, no network,
    and cannot be revoked by anyone but the user. Register with:

        scripts/record-audit.py --print-daily-task

EXIT CODES (a report, not a gate — but still tri-state)
    0  every dimension measured, none over threshold
    1  at least one dimension over threshold (findings recorded to review-queue)
    2  at least one dimension COULD NOT BE MEASURED

    2 outranks 1. An unmeasured dimension is not a passing one: a broken probe
    would otherwise be indistinguishable from a healthy record (CLAUDE.md
    section 3). Nothing here ever substitutes 0 for "the probe failed" — that
    substitution is the exact fault this script was written to detect in others.

    `daily` surfaces any non-zero exit in its session summary line, so a breach
    is visible in-conversation as well as on the review queue.
"""

import argparse
import json
import os
import subprocess
import sys
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DAY = 86400

# --- thresholds ------------------------------------------------------------
# Measured 2026-07-31 at BASELINE_REV, by running this script's own probes:
#
#     python3 scripts/record-audit.py --no-escalate
#
# which prints every dimension's current value next to its threshold. Re-measure
# rather than inherit these (CLAUDE.md: a number with no measurement point rots
# the moment it is copied).
BASELINE_REV = "c3f29681"

# Doc-claim drift is a RATCHET, not an absolute bar: 140 stale claims already
# exist and this job's purpose is not to fix them, it is to stop them growing
# unobserved. The absolute figure is printed on every run regardless of verdict,
# because "140, and the gate calls that clean" is the finding.
DOC_DRIFT_BASELINE = 140

# Any `converging: false` is a breach. There is no tolerance band: the flag is
# already the summary of a trend, so a band on top of it would be a band on a
# band.

# 42 rows were open against 0 dispositions ever recorded. A human review surface
# past ~20 open items is not being reviewed, it is being accumulated.
OPEN_REVIEW_QUEUE_MAX = 20

# `stale_undisposed_with_fix_commit` counts findings whose FIX landed but which
# nobody dispositioned — the queue lying in the safe direction. Measured at 1.
STALE_UNDISPOSED_MAX = 5

# The oldest pending item measured 10 days; the median 6. 21 days is therefore
# comfortably outside normal churn, so a hit means genuinely stuck, not busy.
BACKLOG_STALE_DAYS = 21

# `backlog fail` defers 2 days rather than retiring. An item whose updated_at is
# more than that past its created_at while still `failed` has been failed again
# after resurfacing — the loop this dimension exists to catch. Any occurrence is
# worth a look, so the threshold is 0.
BACKLOG_REFAILED_MAX = 0

FINDING_SOURCE = "record-audit"


# --- the tri-state ---------------------------------------------------------
class Measurement:
    """A number, or an explicit statement that it could not be obtained.

    The Python stand-in for `harness_core::verdict::Determination`. There is no
    `.get(default)` and no truthiness on purpose: every consumer must handle
    `undetermined` by name, so a probe failure cannot be spent as a 0.
    """

    __slots__ = ("value", "why", "detail")

    def __init__(self, value=None, why=None, detail=None):
        if (value is None) == (why is None):
            raise ValueError("a Measurement is exactly one of value / why")
        self.value = value
        self.why = why
        self.detail = detail or {}

    @classmethod
    def known(cls, value, **detail):
        return cls(value=value, detail=detail)

    @classmethod
    def undetermined(cls, why, **detail):
        return cls(why=why, detail=detail)

    @property
    def is_known(self):
        return self.why is None

    def __repr__(self):
        return f"Known({self.value})" if self.is_known else f"Undetermined({self.why})"


class Dimension:
    """One measured record-health axis and its verdict."""

    def __init__(self, key, title, measurement, threshold, breached, severity, note=""):
        self.key = key
        self.title = title
        self.m = measurement
        self.threshold = threshold
        self.breached = breached
        self.severity = severity
        self.note = note

    @property
    def state(self):
        if not self.m.is_known:
            return "undetermined"
        return "breach" if self.breached else "ok"

    def as_dict(self):
        return {
            "key": self.key,
            "title": self.title,
            "state": self.state,
            "value": self.m.value,
            "undetermined_why": self.m.why,
            "threshold": self.threshold,
            "severity": self.severity,
            "detail": self.m.detail,
            "note": self.note,
        }


# --- probes ----------------------------------------------------------------
def _run(cmd, cwd=REPO, timeout=180):
    """Run a probe. Returns (rc, stdout, stderr) or raises RuntimeError.

    The exit status is part of every answer here. A probe that dies must not be
    read for its stdout: an empty stdout from a crashed tool parses as an empty
    result set, which is precisely the "checked and found nothing" lie.
    """
    try:
        p = subprocess.run(
            cmd, cwd=cwd, capture_output=True, text=True, timeout=timeout
        )
    except FileNotFoundError:
        raise RuntimeError(f"{cmd[0]} is not on PATH")
    except subprocess.TimeoutExpired:
        raise RuntimeError(f"{' '.join(cmd)} timed out after {timeout}s")
    except OSError as e:
        raise RuntimeError(f"could not run {' '.join(cmd)}: {e}")
    return p.returncode, p.stdout, p.stderr


def record_root():
    """The path whose RECORDS these are: the main worktree, never a linked one.

    `overwatch` and `backlog` both resolve their stores from the cwd, so a
    linked worktree resolves to a different — and empty — project. Run from
    there, every store-backed dimension reads 0: no queue depth, no undisposed
    findings, no backlog at all. Which is to say it reports a perfectly healthy
    set of records that do not exist. That is precisely the empty-set-reads-as-
    clean fault this script was written to detect in other tools (CLAUDE.md
    section 3), and it was reproduced here first — observed, not predicted, by
    running the job from a worktree and getting `pending_total: 0` against a
    live 87-item queue.

    Returns the path, or None if git could not answer. None is NOT silently
    replaced with REPO: guessing which store to read is how the wrong answer
    gets reported confidently.
    """
    try:
        rc, out, _ = _run(
            ["git", "rev-parse", "--path-format=absolute", "--git-common-dir"], cwd=REPO
        )
    except RuntimeError:
        return None
    if rc != 0 or not out.strip():
        return None
    common = out.strip()
    # `<main>/.git` in both a plain clone and a linked worktree; a bare repo or
    # anything else unexpected is not a record root we can vouch for.
    if os.path.basename(common) != ".git":
        return None
    root = os.path.dirname(common)
    return root if os.path.isdir(root) else None


def _store_cwd():
    root = record_root()
    if root is None:
        raise RuntimeError(
            "could not resolve the record-owning worktree via git; refusing to "
            "read a store that might belong to a different project"
        )
    return root


def _run_json(cmd, cwd=REPO, ok_codes=(0,)):
    """Run a probe that emits JSON. Any failure raises rather than returning {}."""
    rc, out, err = _run(cmd, cwd=cwd)
    if rc not in ok_codes:
        tail = (err or out).strip().splitlines()
        raise RuntimeError(
            f"{' '.join(cmd)} exited {rc}"
            + (f": {tail[-1][:160]}" if tail else "")
        )
    try:
        return json.loads(out)
    except (ValueError, TypeError) as e:
        raise RuntimeError(f"{' '.join(cmd)} did not emit parseable JSON: {e}")


def measure_doc_drift():
    """Stale `path:line` claims across docs/**/*.md and CLAUDE.md.

    Counts EXEMPT findings too — they are the whole point. An exempt drifted
    claim is still a claim that no longer describes the tree; the exemption
    only means it does not block a commit.
    """
    total, by_kind, per_source = 0, {}, {}
    for label, cmd in (
        ("docs", ["python3", "scripts/check-doc-claims.py", "--json"]),
        ("CLAUDE.md", ["python3", "scripts/check-claudemd-claims.py", "--json"]),
    ):
        try:
            d = _run_json(cmd)
        except RuntimeError as e:
            return Measurement.undetermined(f"{label} claim check failed: {e}")
        findings = d.get("findings")
        if not isinstance(findings, list):
            return Measurement.undetermined(
                f"{label} claim check emitted no `findings` list "
                f"(keys: {sorted(d)}) — shape change, not zero drift"
            )
        per_source[label] = len(findings)
        total += len(findings)
        for f in findings:
            k = f.get("kind", "unknown")
            by_kind[k] = by_kind.get(k, 0) + 1
    return Measurement.known(total, by_kind=by_kind, per_source=per_source)


def measure_audit_convergence():
    """The Continuous-Audit round ledger's tri-state `converging` flag."""
    try:
        d = _run_json(["overwatch", "audit-metrics", "--json"], cwd=_store_cwd())
    except RuntimeError as e:
        return Measurement.undetermined(f"overwatch audit-metrics failed: {e}")
    if "converging" not in d:
        return Measurement.undetermined(
            "overwatch audit-metrics emitted no `converging` key"
        )
    conv = d["converging"]
    rounds = d.get("rounds", [])
    if conv is None:
        # overwatch's own third state: too few rounds to judge. Undetermined
        # here as well — "not enough history to say" is not "converging".
        return Measurement.undetermined(
            f"overwatch reports converging=unknown ({len(rounds)} round(s) recorded)",
            rounds=len(rounds),
        )
    if not isinstance(conv, bool):
        return Measurement.undetermined(
            f"`converging` was {conv!r}, not a boolean or null"
        )
    # 1 = converging, 0 = not. Encoded as a number so the ledger's trend column
    # is uniform across dimensions.
    return Measurement.known(
        1 if conv else 0,
        rounds=len(rounds),
        recent=[r.get("new_findings") for r in rounds[-6:]],
        closure_rate=d.get("closure_rate"),
    )


def measure_open_review_queue():
    """How many entries sit unresolved on the human review surface."""
    try:
        rows = _run_json(["overwatch", "review-queue", "--json"], cwd=_store_cwd())
    except RuntimeError as e:
        return Measurement.undetermined(f"overwatch review-queue failed: {e}")
    if not isinstance(rows, list):
        return Measurement.undetermined(
            f"overwatch review-queue emitted {type(rows).__name__}, not a list"
        )
    by_kind = {}
    for r in rows:
        k = r.get("kind", "unknown")
        by_kind[k] = by_kind.get(k, 0) + 1

    # Joined with the disposition ledger so the report can say whether the queue
    # is deep because work is arriving or because nobody is closing it. A
    # failure to read that ledger degrades the DETAIL, not the count — the queue
    # depth above was measured successfully and is reported either way.
    dispositions = None
    stale_undisposed = None
    try:
        rm = _run_json(["overwatch", "review-metrics", "--json"], cwd=_store_cwd())
        dispositions = rm.get("total")
        stale_undisposed = rm.get("stale_undisposed_with_fix_commit")
    except RuntimeError:
        pass
    return Measurement.known(
        len(rows),
        by_kind=by_kind,
        dispositions_recorded=dispositions,
        stale_undisposed=stale_undisposed,
    )


def measure_stale_undisposed():
    """Findings whose fix landed but which nobody dispositioned."""
    try:
        d = _run_json(["overwatch", "review-metrics", "--json"], cwd=_store_cwd())
    except RuntimeError as e:
        return Measurement.undetermined(f"overwatch review-metrics failed: {e}")
    n = d.get("stale_undisposed_with_fix_commit")
    if not isinstance(n, int):
        return Measurement.undetermined(
            f"`stale_undisposed_with_fix_commit` was {n!r}, not an integer"
        )
    return Measurement.known(n, dispositions_recorded=d.get("total"))


def measure_backlog_rot(now, stale_days):
    """Long-pending items, and items that resurfaced after being failed.

    Both are read off the queue itself rather than a counter, because the
    backlog schema has no fail-count. `updated_at` moving past the two-day defer
    window while the status is still `failed` is the observable trace of an item
    that came back and was rejected again.
    """
    try:
        tasks = _run_json(["backlog", "list", "--status", "pending", "--json"],
                          cwd=_store_cwd())
        failed = _run_json(["backlog", "list", "--status", "failed", "--json"],
                           cwd=_store_cwd())
    except RuntimeError as e:
        return Measurement.undetermined(f"backlog list failed: {e}")
    if not isinstance(tasks, list) or not isinstance(failed, list):
        return Measurement.undetermined("backlog list did not emit a JSON array")

    stale, refailed, ages = [], [], []
    for t in tasks:
        created = t.get("created_at")
        if not isinstance(created, int):
            return Measurement.undetermined(
                f"backlog task {t.get('id')!r} has created_at={created!r}; "
                "an unaged item cannot be judged stale or fresh"
            )
        age = (now - created) // DAY
        ages.append(age)
        if age > stale_days:
            stale.append({"id": t.get("id"), "age_days": age, "title": t.get("title")})
    for t in failed:
        created, updated = t.get("created_at"), t.get("updated_at")
        if not isinstance(created, int) or not isinstance(updated, int):
            return Measurement.undetermined(
                f"backlog task {t.get('id')!r} has non-integer timestamps"
            )
        if updated - created > 2 * DAY:
            refailed.append({"id": t.get("id"), "title": t.get("title")})

    ages.sort()
    return Measurement.known(
        len(stale) + len(refailed),
        stale_count=len(stale),
        refailed_count=len(refailed),
        stale_items=stale[:10],
        refailed_items=refailed[:10],
        pending_total=len(tasks),
        oldest_age_days=ages[-1] if ages else 0,
    )


# --- assembly --------------------------------------------------------------
def collect(now, thresholds):
    dims = []

    m = measure_doc_drift()
    dims.append(Dimension(
        "doc-claim-drift",
        "normative-doc claims that no longer match the tree",
        m,
        thresholds["doc_drift"],
        m.is_known and m.value > thresholds["doc_drift"],
        "med",
        note=(
            "ratchet: breach on GROWTH above the baseline. The absolute count is "
            "reported every run because check-doc-claims.py renders an exempt "
            "drifted claim as `clean`."
        ),
    ))

    m = measure_audit_convergence()
    dims.append(Dimension(
        "audit-convergence",
        "Continuous-Audit rounds are finding fewer new problems over time",
        m,
        1,
        m.is_known and m.value < 1,
        "high",
        note="1 = converging, 0 = not. overwatch's own `unknown` maps to undetermined.",
    ))

    m = measure_open_review_queue()
    dims.append(Dimension(
        "review-queue-depth",
        "entries waiting on the human review surface",
        m,
        thresholds["open_queue"],
        m.is_known and m.value > thresholds["open_queue"],
        "med",
    ))

    m = measure_stale_undisposed()
    dims.append(Dimension(
        "stale-undisposed",
        "findings whose fix landed but which nobody dispositioned",
        m,
        thresholds["stale_undisposed"],
        m.is_known and m.value > thresholds["stale_undisposed"],
        "med",
    ))

    m = measure_backlog_rot(now, thresholds["stale_days"])
    dims.append(Dimension(
        "backlog-rot",
        f"items pending over {thresholds['stale_days']}d, plus re-failed resurfacers",
        m,
        thresholds["backlog_rot"],
        m.is_known and m.value > thresholds["backlog_rot"],
        "med",
    ))

    return dims


def render(dims, now, escalated, escalation_note):
    lines = ["record-freshness audit", f"  at: {time.strftime('%Y-%m-%d %H:%M:%S', time.localtime(now))}", ""]
    for d in dims:
        if d.state == "undetermined":
            mark, val = "??", f"COULD NOT MEASURE — {d.m.why}"
        else:
            mark = "!!" if d.breached else "ok"
            val = f"{d.m.value} (threshold {d.threshold})"
        lines.append(f"  [{mark}] {d.key}: {val}")
        lines.append(f"         {d.title}")
        for k, v in sorted(d.m.detail.items()):
            lines.append(f"           {k}: {v}")
        if d.note:
            lines.append(f"         note: {d.note}")
        lines.append("")

    breaches = [d for d in dims if d.state == "breach"]
    undet = [d for d in dims if d.state == "undetermined"]
    lines.append(
        f"  {len(dims) - len(breaches) - len(undet)} ok / {len(breaches)} over "
        f"threshold / {len(undet)} unmeasurable"
    )
    if undet:
        lines.append(
            "  An unmeasurable dimension is NOT a passing one — a broken probe and "
            "a healthy record must not read alike."
        )
    if escalated:
        lines.append(f"  recorded to review-queue: {', '.join(escalated)}")
    if escalation_note:
        lines.append(f"  {escalation_note}")
    return "\n".join(lines)


def already_open(finding_id):
    """Is this finding already unresolved on the queue?

    Delegates "open" to overwatch rather than re-deriving it from the raw
    ledgers: `record-finding` is a plain append, so a daily re-record of an
    unchanged condition would otherwise stack a duplicate row every day and bury
    the surface this job exists to keep readable.

    Returns True / False / None, where None means the queue could not be read.
    The caller does NOT record on None: appending on an unreadable queue is the
    duplicate-stacking failure with no evidence it was needed.
    """
    try:
        rows = _run_json(["overwatch", "review-queue", "--json"], cwd=_store_cwd())
    except RuntimeError:
        return None
    if not isinstance(rows, list):
        return None
    return any(r.get("identifier") == finding_id for r in rows)


def escalate(dims, dry_run):
    """Record each breach onto the review queue. Returns (ids, note)."""
    recorded, skipped, failures = [], [], []
    for d in dims:
        if d.state != "breach":
            continue
        finding_id = f"{FINDING_SOURCE}:{d.key}"
        summary = (
            f"record freshness: {d.key} at {d.m.value} exceeds threshold "
            f"{d.threshold} — {d.title}"
        )
        if dry_run:
            recorded.append(finding_id + " (dry-run)")
            continue
        open_already = already_open(finding_id)
        if open_already is None:
            failures.append(f"{finding_id} (review-queue unreadable; not recorded)")
            continue
        if open_already:
            skipped.append(finding_id)
            continue
        rc, out, err = _run([
            "overwatch", "record-finding",
            "--finding-id", finding_id,
            "--source", FINDING_SOURCE,
            "--severity", d.severity,
            "--summary", summary,
            # Explicit rather than relying on the omitted-flag default, which
            # backlog eda212a0 is in the process of removing.
            "--verdict", "confirmed",
        ])
        if rc != 0:
            failures.append(f"{finding_id} (record-finding exited {rc}: {err.strip()[:120]})")
        else:
            recorded.append(finding_id)

    note = ""
    if skipped:
        note = f"already open, not duplicated: {', '.join(skipped)}"
    if failures:
        note = (note + "; " if note else "") + "COULD NOT RECORD: " + "; ".join(failures)
    return recorded, note


def observation_path():
    root = os.environ.get("RECORD_AUDIT_STATE_DIR") or os.path.join(
        os.path.expanduser("~"), ".record-audit"
    )
    return os.path.join(root, "observations.jsonl")


def append_observation(record):
    """Append one run's numbers to the trend ledger.

    Returns None on success or a reason string on failure. The reason is
    REPORTED rather than swallowed: an audit whose own record silently failed to
    persist is the fault this script exists to detect.
    """
    path = observation_path()
    try:
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "a", encoding="utf-8") as fh:
            fh.write(json.dumps(record, ensure_ascii=False) + "\n")
    except OSError as e:
        return f"could not append the observation to {path}: {e}"
    return None


DAILY_TASK_SNIPPET = """\
# Append to ~/.daily/config.toml — runs at most once per calendar day, at the
# first SessionStart of the day, and skips while a /flow driver holds the lock.
#
# CAREFUL if that file does not exist yet: `daily` falls back to a built-in
# `security` task ONLY while no task is registered (crates/daily/src/main.rs,
# effective_tasks). Creating a config containing just this stanza therefore
# SILENTLY RETIRES the cargo-deny audit. Carry it over explicitly:
#
#   [[task]]
#   name = "security"
#   command = "cargo deny check advisories bans sources licenses"
[[task]]
name = "record-audit"
command = "python3 scripts/record-audit.py"
dir = "{repo}"
"""


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--json", action="store_true", help="emit the report as JSON")
    ap.add_argument("--dry-run", action="store_true",
                    help="measure and report, but record nothing to the review queue")
    ap.add_argument("--no-escalate", action="store_true",
                    help="do not touch the review queue at all (implies --dry-run for it)")
    ap.add_argument("--print-daily-task", action="store_true",
                    help="print the ~/.daily/config.toml stanza that schedules this job")
    ap.add_argument("--now", type=int, default=None,
                    help="unix seconds to treat as now (tests; keeps ages deterministic)")
    ap.add_argument("--doc-drift-baseline", type=int, default=DOC_DRIFT_BASELINE)
    ap.add_argument("--open-queue-max", type=int, default=OPEN_REVIEW_QUEUE_MAX)
    ap.add_argument("--stale-undisposed-max", type=int, default=STALE_UNDISPOSED_MAX)
    ap.add_argument("--backlog-stale-days", type=int, default=BACKLOG_STALE_DAYS)
    ap.add_argument("--backlog-rot-max", type=int, default=BACKLOG_REFAILED_MAX)
    args = ap.parse_args(argv)

    if args.print_daily_task:
        sys.stdout.write(DAILY_TASK_SNIPPET.format(repo=REPO))
        return 0

    now = args.now if args.now is not None else int(time.time())
    thresholds = {
        "doc_drift": args.doc_drift_baseline,
        "open_queue": args.open_queue_max,
        "stale_undisposed": args.stale_undisposed_max,
        "stale_days": args.backlog_stale_days,
        "backlog_rot": args.backlog_rot_max,
    }

    dims = collect(now, thresholds)

    if args.no_escalate:
        escalated, esc_note = [], "escalation suppressed (--no-escalate)"
    else:
        escalated, esc_note = escalate(dims, args.dry_run)

    breaches = [d.key for d in dims if d.state == "breach"]
    undetermined = [d.key for d in dims if d.state == "undetermined"]

    record = {
        "ts": now,
        "rev": _head_rev(),
        "dimensions": [d.as_dict() for d in dims],
        "breached": breaches,
        "undetermined": undetermined,
        "escalated": escalated,
    }
    write_err = None
    if not args.dry_run and not args.no_escalate:
        write_err = append_observation(record)
        if write_err:
            record["observation_write_error"] = write_err

    if args.json:
        print(json.dumps(record, ensure_ascii=False, indent=2))
    else:
        note = esc_note
        if write_err:
            note = (note + "; " if note else "") + write_err
        print(render(dims, now, escalated, note))

    # A failed observation write is itself undetermined territory: the run
    # happened but left no record, so the trend this job exists to produce has a
    # hole in it. Resolve to the restrictive side.
    if undetermined or write_err:
        return 2
    return 1 if breaches else 0


def _head_rev():
    try:
        rc, out, _ = _run(["git", "rev-parse", "--short", "HEAD"])
        return out.strip() if rc == 0 else None
    except RuntimeError:
        return None


if __name__ == "__main__":
    sys.exit(main())
