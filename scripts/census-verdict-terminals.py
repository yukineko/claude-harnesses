#!/usr/bin/env python3
"""Enumerate a gate crate's verdict-terminal sites.

DoD9 (compass charter) asks each gate crate for a per-gate audit whose
completion condition is "no unaudited verdict path". That denominator cannot
come from reading: a human or an LLM cannot observe their own coverage, so
"I read the file and enumerated every path" is a prediction in the sense of
CLAUDE.md 2, not an observation. A script can be observed — it either visited
a line or it did not.

So this produces the DENOMINATOR mechanically and leaves the CLASSIFICATION to
the auditor, which is the only part that needs judgment. It deliberately
over-collects: a site listed and classified "not a verdict path" costs one
line in a table, a site never listed is invisible.

    python3 scripts/census-verdict-terminals.py blastguard
    python3 scripts/census-verdict-terminals.py blastguard --json

Exit status is 0 on success and 2 when the crate's `src/` cannot be read —
"could not enumerate" is not "nothing to enumerate" (CLAUDE.md 3), so it must
not look like a clean census.
"""

import json
import os
import re
import sys

# A site is interesting if it PRODUCES a verdict, or COLLAPSES a fallible value
# into one. Deny/Ask productions are counted too, so the output reports the
# ratio rather than only the alarming half.
PATTERNS = {
    "allow": re.compile(r"Decision::Allow"),
    "deny": re.compile(r"Decision::deny\(|Decision::Deny\("),
    "ask": re.compile(r"Decision::ask\(|Decision::Ask\("),
    "unwrap_or": re.compile(r"\.unwrap_or\("),
    "unwrap_or_else": re.compile(r"\.unwrap_or_else\("),
    "unwrap_or_default": re.compile(r"\.unwrap_or_default\(\)"),
    "ok_erase": re.compile(r"\.ok\(\)"),
    "empty_collection": re.compile(r"Vec::new\(\)|vec!\[\]"),
    "catchall_arm": re.compile(r"^\s*_ =>"),
    "none_arm": re.compile(r"^\s*None(\s+if .*)? =>"),
    "err_arm": re.compile(r"^\s*Err\(.*\) =>"),
    "determination": re.compile(r"Determination::"),
    "require": re.compile(r"\.require\("),
}

# The subset that is the actual audit question: a terminal where "could not
# determine" could become "fine".
COLLAPSING = {
    "allow",
    "unwrap_or",
    "unwrap_or_else",
    "unwrap_or_default",
    "ok_erase",
    "empty_collection",
    "catchall_arm",
    "none_arm",
    "err_arm",
}

FN_RE = re.compile(r"^\s*(?:pub(?:\([a-z]+\))?\s+)?fn\s+([A-Za-z0-9_]+)")
TEST_MOD_RE = re.compile(r"^\s*mod tests\b")


def census_file(path, rel):
    with open(path, encoding="utf-8") as fh:
        lines = fh.read().split("\n")
    # Everything from `mod tests` onward is test code. Classified, not dropped:
    # "the tests pin a fail-open as specification" is itself a finding, and
    # discarding the test half would hide it.
    test_start = next((i for i, l in enumerate(lines) if TEST_MOD_RE.match(l)), len(lines))
    fn = "<file scope>"
    out = []
    for i, line in enumerate(lines):
        m = FN_RE.match(line)
        if m:
            fn = m.group(1)
        stripped = line.strip()
        if stripped.startswith("//"):
            continue  # a comment naming Allow is not a verdict path
        kinds = [k for k, rx in PATTERNS.items() if rx.search(line)]
        if kinds:
            out.append(
                {
                    "file": rel,
                    "line": i + 1,
                    "fn": fn,
                    "in_tests": i >= test_start,
                    "kinds": kinds,
                    "text": stripped,
                }
            )
    return out


def main(argv):
    if len(argv) < 2:
        print(__doc__, file=sys.stderr)
        return 2
    crate = argv[1]
    as_json = "--json" in argv

    repo = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    src = os.path.join(repo, "crates", crate, "src")
    try:
        names = sorted(n for n in os.listdir(src) if n.endswith(".rs"))
    except OSError as exc:
        # Fail closed: an unreadable tree is not an empty one.
        print("census: cannot read %s: %s" % (src, exc), file=sys.stderr)
        return 2
    if not names:
        print("census: no .rs files under %s" % src, file=sys.stderr)
        return 2

    rows = []
    for n in names:
        try:
            rows.extend(census_file(os.path.join(src, n), n))
        except OSError as exc:
            print("census: cannot read %s: %s" % (n, exc), file=sys.stderr)
            return 2

    if as_json:
        json.dump(rows, sys.stdout, ensure_ascii=False, indent=1)
        sys.stdout.write("\n")
        return 0

    prod = [r for r in rows if not r["in_tests"]]
    print(
        "=== %s: %d production verdict-terminal sites (%d in tests) ==="
        % (crate, len(prod), len(rows) - len(prod))
    )
    by_file = {}
    for r in prod:
        by_file.setdefault(r["file"], []).append(r)
    for f in names:
        rs = by_file.get(f, [])
        if not rs:
            continue
        kinds = {}
        for r in rs:
            for k in r["kinds"]:
                kinds[k] = kinds.get(k, 0) + 1
        print("\n%-16s %3d sites  %s" % (f, len(rs), json.dumps(kinds, sort_keys=True)))

    susp = [r for r in prod if COLLAPSING & set(r["kinds"])]
    print("\n=== permissive-or-collapsing terminals to classify: %d ===" % len(susp))
    for r in susp:
        print("%s:%d  %-34s %s" % (r["file"], r["line"], r["fn"], r["text"][:92]))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
