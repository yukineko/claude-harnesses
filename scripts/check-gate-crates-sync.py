#!/usr/bin/env python3
"""Verify the GATE_CRATES crate set is consistent across its 4 hardcoded sources.

The set of "GATE crates" (fleet defense gates that require a canary rollout) is
hardcoded in four separate places. Canonical direction: scripts/rollout-plugins.sh's
GATE_CRATES= line is the source of truth; the other three must never drift from it:
  - scripts/rollout-plugins.sh    GATE_CRATES="..."           (space-separated, canonical)
  - scripts/continuous-audit.sh   DEFAULT_TARGETS="..."       (comma-separated)
  - .githooks/pre-push            GATE_PATTERN='...'          (regex alternation)
  - crates/overwatch/skills/continuous-audit/SKILL.md  "## 対象 crate (既定)" section
    (backtick-quoted comma-separated list)

See docs/fix-gate-crates-drift.md for the incident that motivated this checker.

Exit 0 if all four sets agree, 1 if any source drifts from the canonical set.
Run from the repo root:  python3 scripts/check-gate-crates-sync.py
"""
import os
import re
import sys

REPO = os.getcwd()


def canonical_crates(text):
    """Extract the GATE_CRATES="..." value (space-separated) from rollout-plugins.sh."""
    m = re.search(r'^GATE_CRATES="([^"]+)"', text, re.M)
    if not m:
        return None
    return set(m.group(1).split())


def continuous_audit_crates(text):
    """Extract the DEFAULT_TARGETS="..." value (comma-separated) from continuous-audit.sh."""
    m = re.search(r'^DEFAULT_TARGETS="([^"]+)"', text, re.M)
    if not m:
        return None
    return set(x for x in m.group(1).split(",") if x)


def pre_push_crates(text):
    """Extract crate names from the GATE_PATTERN='^crates/(a|b|c)/' regex."""
    m = re.search(r"^GATE_PATTERN='\^crates/\(([^)]+)\)/'", text, re.M)
    if not m:
        return None
    return set(m.group(1).split("|"))


def skill_md_crates(text):
    """Extract crate names from the "## 対象 crate (既定)" section's backtick CSV."""
    m = re.search(r"##\s*対象\s*crate\s*\(既定\)\s*\n+.*?`([a-z0-9_,-]+)`", text, re.S)
    if not m:
        return None
    return set(x for x in m.group(1).split(",") if x)


SOURCES = [
    ("scripts/rollout-plugins.sh", canonical_crates, True),
    ("scripts/continuous-audit.sh", continuous_audit_crates, False),
    (".githooks/pre-push", pre_push_crates, False),
    ("crates/overwatch/skills/continuous-audit/SKILL.md", skill_md_crates, False),
]


def check(repo=REPO, sources=SOURCES):
    """Return (ok, canonical_set, [(path, extracted_set_or_None), ...]) for the given repo."""
    parsed = []
    canonical = None
    for rel_path, extractor, is_canonical in sources:
        path = os.path.join(repo, rel_path)
        if not os.path.isfile(path):
            parsed.append((rel_path, None))
            continue
        with open(path, encoding="utf-8") as f:
            text = f.read()
        crates = extractor(text)
        parsed.append((rel_path, crates))
        if is_canonical:
            canonical = crates

    if canonical is None:
        return False, None, parsed

    ok = all(crates == canonical for _, crates in parsed if crates is not None) and all(
        crates is not None for _, crates in parsed
    )
    return ok, canonical, parsed


def main():
    ok, canonical, parsed = check(repo=os.getcwd())
    if canonical is None:
        print("check-gate-crates-sync: could not parse canonical GATE_CRATES from "
              "scripts/rollout-plugins.sh", file=sys.stderr)
        return 1

    if ok:
        print(f"OK: GATE_CRATES consistent across {len(parsed)} sources: "
              f"{','.join(sorted(canonical))}")
        return 0

    print("FAIL: GATE_CRATES definition drift detected", file=sys.stderr)
    print(f"  canonical (scripts/rollout-plugins.sh): {sorted(canonical)}", file=sys.stderr)
    for rel_path, crates in parsed:
        if crates is None:
            print(f"  {rel_path}: could not parse a crate set", file=sys.stderr)
        elif crates != canonical:
            missing = canonical - crates
            extra = crates - canonical
            detail = []
            if missing:
                detail.append(f"missing {sorted(missing)}")
            if extra:
                detail.append(f"unexpected {sorted(extra)}")
            print(f"  {rel_path}: {sorted(crates)} ({'; '.join(detail)})", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
