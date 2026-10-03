#!/usr/bin/env python3
"""CI regression gate for the benchkit SWE-bench dashboard.

The benchkit dashboard appends one JSON line per harness run to a `runs.jsonl`
store (see crates/benchkit/src/dashboard.rs). Each line is a RunRecord with the
fields (exact names, snake_case, no serde rename):
  - timestamp        i64 epoch seconds
  - resolution_rate  float in [0.0, 1.0]  (fraction of instances resolved)
  - instances        list of {instance_id, resolved}
  - model            string
  - cost             float

This gate compares the LATEST run's resolution_rate against a baseline and fails
(exit 1) when the rate dropped by more than an allowed threshold — a resolution
regression. The baseline is, by default, the immediately preceding entry in the
JSONL; it can instead be pinned to a specific run (by 0-based index or negative
index) or read from a separate baseline file.

Three answers, not two (CLAUDE.md 3, backlog f6919056). "Could not compare" is
not "no regression": a missing or unreadable dashboard, a missing / out-of-range
baseline, or a run without a usable resolution_rate exits 2 (UNDETERMINED), never 0.

The threshold is clamped into [0.0, MAX_THRESHOLD]. resolution_rate is bounded to
[0.0, 1.0], so an unclamped threshold of 1.0 or more could never be exceeded and
silently disabled the gate; a non-finite threshold is rejected as UNDETERMINED.

Exit 0 = compared, no regression; exit 1 = regression detected;
exit 2 = undetermined (nothing could be compared).
Stdlib only (argparse, json, sys). Run from the repo root:
  python3 scripts/check-bench-regression.py --dashboard <path> --threshold 0.05
"""
import argparse
import json
import math
import sys

EXIT_OK = 0
EXIT_REGRESSION = 1
EXIT_UNDETERMINED = 2

# Largest allowed drop. Must stay well below 1.0 (the full range of
# resolution_rate) or the gate can never fire.
MAX_THRESHOLD = 0.5


def resolution_rate_regressed(latest_rate, baseline_rate, threshold):
    """Return True iff the resolution rate dropped by more than `threshold`.

    Pure helper: `(baseline_rate - latest_rate) > threshold`. An improvement or a
    drop within the allowed threshold returns False.
    """
    return (baseline_rate - latest_rate) > threshold


def load_runs(path):
    """Read a benchkit dashboard JSONL file into a list of run records.

    Blank lines are skipped. A missing file yields an empty list; the caller
    reports that as UNDETERMINED, not as "no regression". Unreadable or
    malformed content raises (OSError / ValueError) for the caller to report.
    """
    runs = []
    try:
        with open(path, encoding="utf-8") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                runs.append(json.loads(line))
    except FileNotFoundError:
        return []
    return runs


def parse_args(argv=None):
    p = argparse.ArgumentParser(
        description="Fail CI when the latest benchkit run's resolution_rate "
        "regressed beyond --threshold versus a baseline."
    )
    p.add_argument(
        "--dashboard",
        required=True,
        help="Path to the benchkit dashboard runs.jsonl store.",
    )
    p.add_argument(
        "--baseline",
        default=None,
        help="Baseline to compare the latest run against. Either a separate "
        "JSONL file path (its LAST run is used) or an integer index into the "
        "--dashboard runs (e.g. 0 for the first, -2 for the second-to-last). "
        "Default: the immediately preceding entry in --dashboard.",
    )
    p.add_argument(
        "--threshold",
        type=float,
        default=0.05,
        help="Allowed resolution_rate drop before it counts as a regression "
        "(default: 0.05).",
    )
    return p.parse_args(argv)


def _rate(record):
    """Extract a usable resolution_rate (finite, within [0, 1]) or None."""
    if not isinstance(record, dict):
        return None
    rate = record.get("resolution_rate")
    if isinstance(rate, bool) or not isinstance(rate, (int, float)):
        return None
    if not math.isfinite(rate) or not 0.0 <= rate <= 1.0:
        return None
    return float(rate)


def clamp_threshold(threshold):
    """Clamp into [0, MAX_THRESHOLD]; None for a non-finite value."""
    if not math.isfinite(threshold):
        return None
    return min(max(threshold, 0.0), MAX_THRESHOLD)


def resolve_baseline(runs, baseline_arg):
    """Pick the baseline record given the --baseline argument.

    Returns (baseline_record_or_None, description). Handles:
      - default (None): the immediately preceding entry (runs[-2]),
      - integer-like string: index into `runs`,
      - otherwise: a path to a separate JSONL file whose LAST run is the baseline.
    A None record means "no baseline available" (caller reports UNDETERMINED).
    """
    if baseline_arg is None:
        if len(runs) < 2:
            return None, "preceding entry (none available)"
        return runs[-2], "immediately preceding entry"

    # An integer (possibly negative) selects a run by index within --dashboard.
    try:
        idx = int(baseline_arg)
    except ValueError:
        idx = None
    if idx is not None:
        if -len(runs) <= idx < len(runs):
            return runs[idx], f"--dashboard run at index {idx}"
        return None, f"--dashboard run at index {idx} (out of range)"

    # Otherwise treat --baseline as a separate JSONL file; use its last run.
    baseline_runs = load_runs(baseline_arg)
    if not baseline_runs:
        return None, f"baseline file {baseline_arg} (empty or missing)"
    return baseline_runs[-1], f"last run of baseline file {baseline_arg}"


def main(argv=None):
    args = parse_args(argv)

    def undetermined(why):
        print(f"UNDETERMINED: {why} -> nothing was compared", file=sys.stderr)
        return EXIT_UNDETERMINED

    threshold = clamp_threshold(args.threshold)
    if threshold is None:
        return undetermined(f"--threshold {args.threshold} is not a finite number")
    if threshold != args.threshold:
        print(
            f"note: --threshold {args.threshold} clamped to {threshold} "
            f"(allowed range 0.0..{MAX_THRESHOLD})",
            file=sys.stderr,
        )

    try:
        runs = load_runs(args.dashboard)
    except (OSError, ValueError) as e:
        return undetermined(f"dashboard {args.dashboard} unreadable ({e})")
    if not runs:
        return undetermined(f"dashboard {args.dashboard} is empty or missing (no runs)")

    latest_rate = _rate(runs[-1])
    if latest_rate is None:
        return undetermined(
            f"latest run in {args.dashboard} has no usable resolution_rate"
        )

    try:
        baseline, desc = resolve_baseline(runs, args.baseline)
    except (OSError, ValueError) as e:
        return undetermined(f"baseline {args.baseline} unreadable ({e})")
    baseline_rate = _rate(baseline)
    if baseline_rate is None:
        return undetermined(f"no usable baseline ({desc})")

    drop = baseline_rate - latest_rate
    if resolution_rate_regressed(latest_rate, baseline_rate, threshold):
        print(
            "REGRESSION: resolution_rate dropped "
            f"{drop:.4f} (baseline {baseline_rate:.4f} [{desc}] -> "
            f"latest {latest_rate:.4f}), exceeds threshold {threshold:.4f}",
            file=sys.stderr,
        )
        return EXIT_REGRESSION

    print(
        f"OK: resolution_rate {latest_rate:.4f} vs baseline {baseline_rate:.4f} "
        f"[{desc}] (drop {drop:.4f} <= threshold {threshold:.4f})"
    )
    return EXIT_OK


if __name__ == "__main__":
    sys.exit(main())
