#!/usr/bin/env python3
"""Verify the GATE_CRATES crate set is consistent across its 4 hardcoded sources.

Two related-but-distinct concepts are hardcoded across these sources:
  - "GATE crates": fleet defense gates that require a canary rollout
    (scripts/rollout-plugins.sh's GATE_CRATES= line is the source of truth).
  - "audit targets": crates continuous-audit reviews by default. This is a
    strict SUPERSET of GATE crates — it may include audit-only crates (e.g.
    `backlog`) that get reviewed but do not gate/block anything and so are
    NOT GATE crates (no canary requirement, not in pre-push's GATE_PATTERN).

Sources and how each must relate to the canonical GATE_CRATES set:
  - scripts/rollout-plugins.sh    GATE_CRATES="..."     (space-separated, canonical)
  - .githooks/pre-push            GATE_PATTERN='...'    (regex alternation) — must
    equal canonical EXACTLY (pre-push's canary advisory is GATE-crates-only).
  - scripts/continuous-audit.sh   DEFAULT_TARGETS="..." (comma-separated) — must be
    a SUPERSET of canonical (the audit target set; may include non-GATE crates).
  - crates/overwatch/skills/continuous-audit/SKILL.md  "## 対象 crate (既定)" section
    (comma-separated list after "既定の target は") — must equal
    scripts/continuous-audit.sh's DEFAULT_TARGETS EXACTLY (the doc must describe
    what the script actually defaults to, whatever audit-only crates it has).

See docs/fix-gate-crates-drift.md for the incident that motivated this checker.

Exit 0 if all sources satisfy their required relation, 1 on any drift.
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


# mode:
#   "canonical" — this source defines the canonical GATE_CRATES set.
#   "exact"     — must equal canonical exactly (no extra, no missing).
#   "superset"  — must be a superset of canonical (may include audit-only
#                 crates that are reviewed but are not GATE crates).
#   "mirror:<path>" — must equal the named source's parsed set exactly
#                 (a doc describing what that source actually contains).
SOURCES = [
    ("scripts/rollout-plugins.sh", canonical_crates, "canonical"),
    (".githooks/pre-push", pre_push_crates, "exact"),
    ("scripts/continuous-audit.sh", continuous_audit_crates, "superset"),
    (
        "crates/overwatch/skills/continuous-audit/SKILL.md",
        skill_md_crates,
        "mirror:scripts/continuous-audit.sh",
    ),
]


def check(repo=REPO, sources=SOURCES):
    """Return (ok, canonical_set, [(path, mode, extracted_set_or_None), ...]) for the given repo."""
    parsed = []
    by_path = {}
    canonical = None
    for rel_path, extractor, mode in sources:
        path = os.path.join(repo, rel_path)
        if not os.path.isfile(path):
            parsed.append((rel_path, mode, None))
            by_path[rel_path] = None
            continue
        with open(path, encoding="utf-8") as f:
            text = f.read()
        crates = extractor(text)
        parsed.append((rel_path, mode, crates))
        by_path[rel_path] = crates
        if mode == "canonical":
            canonical = crates

    if canonical is None:
        return False, None, parsed

    def satisfies(crates, mode):
        if crates is None:
            return False
        if mode in ("canonical", "exact"):
            return crates == canonical
        if mode == "superset":
            return canonical <= crates
        if mode.startswith("mirror:"):
            target = by_path.get(mode.split(":", 1)[1])
            return target is not None and crates == target
        raise ValueError(f"unknown mode: {mode}")

    ok = all(satisfies(crates, mode) for _, mode, crates in parsed)
    return ok, canonical, parsed


def _mismatch_detail(crates, mode, canonical, by_path):
    if crates is None:
        return "could not parse a crate set"
    if mode in ("canonical", "exact"):
        missing = canonical - crates
        extra = crates - canonical
    elif mode == "superset":
        missing = canonical - crates
        extra = set()
    elif mode.startswith("mirror:"):
        target = by_path.get(mode.split(":", 1)[1]) or set()
        missing = target - crates
        extra = crates - target
    else:
        return "unknown mode"
    detail = []
    if missing:
        detail.append(f"missing {sorted(missing)}")
    if extra:
        detail.append(f"unexpected {sorted(extra)}")
    return "; ".join(detail) if detail else "ok"


def main():
    ok, canonical, parsed = check(repo=os.getcwd())
    if canonical is None:
        print("check-gate-crates-sync: could not parse canonical GATE_CRATES from "
              "scripts/rollout-plugins.sh", file=sys.stderr)
        return 1

    if ok:
        audit_targets = next(
            (crates for path, mode, crates in parsed if mode == "superset"), canonical
        )
        print(f"OK: GATE_CRATES consistent across {len(parsed)} sources: "
              f"{','.join(sorted(canonical))} (audit targets: {','.join(sorted(audit_targets))})")
        return 0

    by_path = {rel_path: crates for rel_path, _mode, crates in parsed}
    print("FAIL: GATE_CRATES definition drift detected", file=sys.stderr)
    print(f"  canonical (scripts/rollout-plugins.sh): {sorted(canonical)}", file=sys.stderr)
    for rel_path, mode, crates in parsed:
        detail = _mismatch_detail(crates, mode, canonical, by_path)
        if detail != "ok":
            shown = sorted(crates) if crates is not None else None
            print(f"  {rel_path} [{mode}]: {shown} ({detail})", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
