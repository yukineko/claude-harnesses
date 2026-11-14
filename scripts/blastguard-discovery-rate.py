#!/usr/bin/env python3
"""Weekly discovery rate of blastguard backlog items: fail-open vs false-positive.

Purpose
-------
Observe whether the rate at which blastguard fail-opens are *discovered* decays
after the allowlist switch ruled on 2026-11-14 (backlog 596ef57d, Q1). For each
ISO week (UTC, labelled ``YYYY-Www``) it counts the blastguard backlog items
newly filed that week (bucketed by ``created_at``), split into:

  * ``fail_open``    -- tag ``fail-open``, or ``fail[- ]open`` in title or notes
  * ``fp``           -- tag ``fp`` / ``false-positive``, or ``\\bFP\\b`` /
                        ``false[- ]positive`` / ``誤検知`` in the title
  * ``ambiguous``    -- matches both of the above
  * ``unclassified`` -- matches neither

Classification is a deterministic tag/title/notes regex, nothing more.
``ambiguous`` and ``unclassified`` are shown explicitly rather than guessed into
one of the other columns. Items without a numeric ``created_at`` go to an
``undated`` bucket (listed last, never dropped). An item is "blastguard" when it
carries the ``blastguard`` tag or its title starts with ``blastguard``
(case-insensitive). Cancelled items are excluded from the weeks but counted in
``excluded_cancelled``.

How to run
----------
    python3 scripts/blastguard-discovery-rate.py --weeks 8
    python3 scripts/blastguard-discovery-rate.py --json
    python3 scripts/blastguard-discovery-rate.py --input tasks.json --json

Without ``--input`` it runs ``backlog list --json --all`` itself.

Fail-closed
-----------
An unreadable / invalid / non-array input, a malformed task record, or a
``backlog`` subprocess that is missing or exits non-zero (its exit status is
checked, not just its stdout) ends with exit 2 and a message on stderr; nothing
that looks like a result is printed to stdout. Only a successfully read input
that selects nothing yields exit 0 with empty weeks.

Downstream consumers
--------------------
A human reading the trend. No gate consumes this output; if one ever does, it
must re-check that the classification is fit to be a verdict input.
"""
import argparse
import datetime
import json
import re
import subprocess
import sys

EXIT_UNDETERMINED = 2

FP_TAGS = {"fp", "false-positive"}
FAIL_OPEN_TAGS = {"fail-open"}
FP_TITLE_RE = re.compile(r"\bFP\b|false[- ]positive|誤検知", re.IGNORECASE)
FAIL_OPEN_RE = re.compile(r"fail[- ]open", re.IGNORECASE)

COLUMNS = ("fail_open", "fp", "ambiguous", "unclassified", "total")
UNDATED = "undated"


class Undetermined(Exception):
    """The input could not be read or interpreted; no result may be printed."""


def load_input(path):
    if path is not None:
        try:
            with open(path, encoding="utf-8") as f:
                text = f.read()
        except OSError as e:
            raise Undetermined("cannot read --input %s: %s" % (path, e))
        source = path
    else:
        try:
            r = subprocess.run(["backlog", "list", "--json", "--all"],
                               capture_output=True, text=True)
        except OSError as e:
            raise Undetermined("cannot run `backlog list --json --all`: %s" % e)
        if r.returncode != 0:
            raise Undetermined("`backlog list --json --all` exited %d: %s"
                               % (r.returncode, r.stderr.strip()))
        text = r.stdout
        source = "backlog list --json --all"
    try:
        data = json.loads(text)
    except ValueError as e:
        raise Undetermined("invalid JSON from %s: %s" % (source, e))
    if not isinstance(data, list):
        raise Undetermined("expected a JSON array from %s, got %s"
                           % (source, type(data).__name__))
    return data


def _str_field(t, key):
    v = t.get(key)
    if v is None:
        return ""
    if not isinstance(v, str):
        raise Undetermined("task %r: field %r is not a string" % (t.get("id"), key))
    return v


def _tags(t):
    v = t.get("tags")
    if v is None:
        return []
    if not isinstance(v, list) or not all(isinstance(x, str) for x in v):
        raise Undetermined("task %r: field 'tags' is not a list of strings" % (t.get("id"),))
    return [x.lower() for x in v]


def is_blastguard(title, tags):
    return "blastguard" in tags or title.lower().startswith("blastguard")


def classify(title, notes, tags):
    fp = bool(FP_TAGS.intersection(tags)) or bool(FP_TITLE_RE.search(title))
    fo = (bool(FAIL_OPEN_TAGS.intersection(tags)) or bool(FAIL_OPEN_RE.search(title))
          or bool(FAIL_OPEN_RE.search(notes)))
    if fp and fo:
        return "ambiguous"
    if fp:
        return "fp"
    if fo:
        return "fail_open"
    return "unclassified"


def week_label(created_at):
    if isinstance(created_at, bool) or not isinstance(created_at, (int, float)):
        return UNDATED
    try:
        dt = datetime.datetime.fromtimestamp(created_at, datetime.timezone.utc)
    except (OverflowError, OSError, ValueError):
        return UNDATED
    iso = dt.isocalendar()
    return "%04d-W%02d" % (iso[0], iso[1])


def aggregate(tasks, weeks_limit):
    buckets = {}
    excluded_cancelled = 0
    for t in tasks:
        if not isinstance(t, dict):
            raise Undetermined("task record is not a JSON object: %r" % (t,))
        title = _str_field(t, "title")
        notes = _str_field(t, "notes")
        tags = _tags(t)
        if not is_blastguard(title, tags):
            continue
        if t.get("status") == "cancelled":
            excluded_cancelled += 1
            continue
        lbl = week_label(t.get("created_at"))
        row = buckets.setdefault(lbl, dict.fromkeys(COLUMNS, 0))
        row[classify(title, notes, tags)] += 1
        row["total"] += 1
    dated = sorted(k for k in buckets if k != UNDATED)
    if weeks_limit is not None:
        dated = dated[-weeks_limit:]
    order = dated + ([UNDATED] if UNDATED in buckets else [])
    weeks = [dict(week=k, **buckets[k]) for k in order]
    return {"weeks": weeks, "excluded_cancelled": excluded_cancelled}


def render_table(result):
    lines = ["%-9s %9s %5s %9s %12s %5s" % ("week", "fail_open", "fp", "ambiguous",
                                            "unclassified", "total")]
    for w in result["weeks"]:
        lines.append("%-9s %9d %5d %9d %12d %5d" % (w["week"], w["fail_open"], w["fp"],
                                                    w["ambiguous"], w["unclassified"],
                                                    w["total"]))
    if not result["weeks"]:
        lines.append("(no blastguard items selected)")
    lines.append("excluded_cancelled: %d" % result["excluded_cancelled"])
    return "\n".join(lines)


def positive_int(s):
    try:
        n = int(s)
    except ValueError:
        raise argparse.ArgumentTypeError("not an integer: %r" % s)
    if n < 1:
        raise argparse.ArgumentTypeError("--weeks must be >= 1, got %d" % n)
    return n


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--input", help="JSON array as from `backlog list --json --all` "
                                    "(default: run that command)")
    ap.add_argument("--json", action="store_true", help="emit JSON instead of a table")
    ap.add_argument("--weeks", type=positive_int, default=None,
                    help="keep only the last N dated weeks (undated is always kept)")
    args = ap.parse_args(argv)
    try:
        result = aggregate(load_input(args.input), args.weeks)
    except Undetermined as e:
        print("blastguard-discovery-rate: UNDETERMINED: %s" % e, file=sys.stderr)
        return EXIT_UNDETERMINED
    if args.json:
        print(json.dumps(result, ensure_ascii=False, indent=2))
    else:
        print(render_table(result))
    return 0


if __name__ == "__main__":
    sys.exit(main())
