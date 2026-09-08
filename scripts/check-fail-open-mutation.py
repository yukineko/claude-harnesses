#!/usr/bin/env python3
"""Adversarial fail-open mutation harness for the 6 GATE crates.

The other fail-open defenses in this repo are STATIC: `check-fail-open.py`
greps for known swallow-shapes; code review reads doc comments and asserts
"this looks fail-closed". Neither of those actually PROVES that a fail-open
regression would be caught by the crate's own test suite — a static grep can
miss a shape it doesn't know about, and "I read the code and it looks right"
is exactly the unverified *judgment* the repo's top-level doctrine (see
CLAUDE.md §2, "判断は予測にすぎない") warns against substituting for an
observed fact.

This script is the missing DYNAMIC check: for each of the 6 GATE crates
(blastguard / propguard / specguard / stuckguard / mutategate / overwatch) it

  1. applies one concrete, mechanical fail-open mutation to a real source
     file (a literal old-string -> new-string replacement, applied
     programmatically — never "an LLM decides at check-time whether this
     would break something"),
  2. runs `cargo test -p <crate>` and captures the real exit code,
  3. asserts the exit code is non-zero (i.e., the crate's OWN test suite
     went RED because of the injected fail-open), and
  4. reverts the mutation (`git checkout -- <file>`), VERIFYING the file is
     byte-identical to its pre-mutation state afterward — this happens in a
     `finally` block, so a revert always runs even if step 2/3 raised or the
     process was interrupted mid-way through a single scenario.

If a mutation compiles but the test suite does NOT go red, that is not
"this scenario failed" — it is a genuine finding (the crate's tests do not
catch that fail-open shape) and is reported as such, never silently swapped
for an easier mutation. If a mutation fails to even compile, that scenario is
INCONCLUSIVE (it doesn't exercise the test suite's semantic judgment at all)
and is reported separately from both CAUGHT and NOT-CAUGHT.

Exit codes:
  0 — every scenario across all 6 crates was CAUGHT (red confirmed).
  1 — at least one scenario was NOT CAUGHT (existing tests missed a real
      fail-open) or INCONCLUSIVE (mutation didn't compile), or an
      unexpected error occurred. Details are always printed; the mutated
      file is always reverted first.

Usage:
  python3 scripts/check-fail-open-mutation.py
  python3 scripts/check-fail-open-mutation.py --crate mutategate   # one crate only
  python3 scripts/check-fail-open-mutation.py --keep-going         # don't stop at first NOT-CAUGHT

GATE_CRATES here is one more hardcoded copy of the canonical 6-crate list
(unavoidable: this is a standalone Python script, not Rust, so it cannot
`pub use harness_core::fleet::GATE_CRATES`). It is registered as an "exact"
source in `scripts/check-gate-crates-sync.py`'s SOURCES list so drift between
this copy and the canonical list is caught the same mechanical way every
other copy is.
"""
import argparse
import subprocess
import sys
import time
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# The canonical 6 GATE crates (see crates/harness-core/src/fleet.rs and
# scripts/check-gate-crates-sync.py). Kept as a plain, greppable tuple of
# string literals — see check-gate-crates-sync.py's SOURCES registration.
GATE_CRATES = (
    "blastguard",
    "propguard",
    "specguard",
    "stuckguard",
    "mutategate",
    "overwatch",
    "parallelguard",
)


class Scenario:
    """One concrete, mechanical fail-open mutation for one crate.

    `old` must appear EXACTLY ONCE in the target file (checked before
    mutating) so the replacement is unambiguous — if it appears zero or more
    than once, the scenario is treated as INCONCLUSIVE rather than guessing.
    """

    def __init__(self, crate, file_rel, description, old, new):
        self.crate = crate
        self.file_rel = file_rel
        self.description = description
        self.old = old
        self.new = new


SCENARIOS = [
    Scenario(
        crate="blastguard",
        file_rel="crates/blastguard/src/model.rs",
        description=(
            "Decision::hardened(): make an unanswerable Ask collapse to Allow "
            "instead of Deny (the exact fail-open the doc comment says this "
            "function exists to prevent)."
        ),
        old="            Decision::Ask(reason) => Decision::Deny(reason),\n",
        new="            Decision::Ask(_reason) => Decision::Allow,\n",
    ),
    Scenario(
        crate="propguard",
        file_rel="crates/propguard/src/gate.rs",
        description=(
            "below_threshold(): hardcode the below-threshold block gate to "
            "always report NOT below threshold, i.e. never block regardless "
            "of satisfied/threshold."
        ),
        old="pub fn below_threshold(satisfied: usize, threshold: usize) -> bool {\n"
        "    satisfied < threshold\n"
        "}\n",
        new="pub fn below_threshold(_satisfied: usize, _threshold: usize) -> bool {\n"
        "    false\n"
        "}\n",
    ),
    Scenario(
        crate="specguard",
        file_rel="crates/specguard/src/parse.rs",
        description=(
            "parse(): flip the fail-closed default for an unparseable/absent "
            "needs_user verdict from (true, true) [surface + indeterminate] "
            "to (false, false) [silently clean] — the exact fail-open #7 the "
            "surrounding comment documents as fixed."
        ),
        old="            (true, true)\n",
        new="            (false, false)\n",
    ),
    Scenario(
        crate="stuckguard",
        file_rel="crates/stuckguard/src/detect.rs",
        description=(
            "repeat(): make the repeat-threshold check always report 'not "
            "enough repeats yet', so the repeat detector never trips "
            "regardless of how many times an action repeats."
        ),
        old="    if same.len() < cfg.repeat_threshold {\n        return None;\n    }\n",
        new="    if true {\n        return None;\n    }\n",
    ),
    Scenario(
        crate="mutategate",
        file_rel="crates/mutategate/src/lib.rs",
        description=(
            "evaluate(): hardcode the kill-rate-vs-threshold comparison to "
            "always pass, so a measured kill-rate below the configured "
            "threshold is reported as passing."
        ),
        old="            let passed = kr + KILL_RATE_EPSILON >= threshold;\n",
        new="            let passed = true;\n",
    ),
    Scenario(
        crate="overwatch",
        file_rel="crates/overwatch/src/canary.rs",
        description=(
            "decide_from_count(): hardcode the canary health gate to always "
            "Proceed, so a violation spike above the configured threshold "
            "never triggers a Rollback."
        ),
        old="    let decision = if observed_violations > policy.max_violations_in_window {\n"
        "        GateDecision::Rollback\n"
        "    } else {\n"
        "        GateDecision::Proceed\n"
        "    };\n",
        new="    let _ = observed_violations;\n"
        "    let _ = policy.max_violations_in_window;\n"
        "    let decision = GateDecision::Proceed;\n",
    ),
]

assert {s.crate for s in SCENARIOS} <= set(GATE_CRATES)


class ScenarioResult:
    def __init__(self, scenario):
        self.scenario = scenario
        self.status = None  # "caught" | "not-caught" | "inconclusive" | "error"
        self.detail = ""
        self.red_confirmed = False
        # Default True ("nothing to revert"): only the code paths that
        # actually apply a mutation set this to the real outcome of
        # `revert_mutation`. The one path that returns BEFORE ever touching
        # the file (the pre-flight dirty-file refusal) must not be reported
        # as "REVERT FAILED" -- no mutation was ever applied, so there is
        # nothing to revert and the pre-existing (unrelated) dirty state was
        # correctly left untouched.
        self.revert_confirmed = True
        self.elapsed = 0.0


def run(cmd, cwd=None, timeout=600):
    # `cwd=None` (resolved to the module-level REPO here, inside the call)
    # rather than a `cwd=REPO` default: a default expression is bound ONCE at
    # function-definition time, so a `cwd=REPO` default would silently ignore
    # any later reassignment of the module-level REPO (e.g. tests pointing
    # this script at a scratch repo) -- callers would keep hitting the real
    # repo on disk no matter what REPO was reassigned to.
    return subprocess.run(
        cmd,
        cwd=str(cwd if cwd is not None else REPO),
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        timeout=timeout,
    )


def git_is_clean(path_rel):
    """True iff `git status --porcelain` reports no changes for this path."""
    p = run(["git", "status", "--porcelain", "--", path_rel])
    return p.stdout.strip() == ""


def git_file_bytes(path):
    return path.read_bytes()


def apply_mutation(scenario):
    """Apply the mutation. Returns None on success, or an error string.

    Requires `old` to appear EXACTLY once (ambiguity is treated as a hard
    stop, never a guess at which occurrence to mutate).
    """
    path = REPO / scenario.file_rel
    text = path.read_text(encoding="utf-8")
    count = text.count(scenario.old)
    if count != 1:
        return (
            f"expected exactly 1 occurrence of the mutation anchor in "
            f"{scenario.file_rel}, found {count} — refusing to guess"
        )
    mutated = text.replace(scenario.old, scenario.new, 1)
    path.write_text(mutated, encoding="utf-8")
    return None


def revert_mutation(scenario, original_bytes):
    """Revert via `git checkout --`, then verify byte-identity. Always run,
    even when the caller is unwinding from an exception (call from finally).
    Returns True iff the revert is confirmed byte-identical.
    """
    path = REPO / scenario.file_rel
    run(["git", "checkout", "--", scenario.file_rel])
    try:
        return path.read_bytes() == original_bytes
    except OSError:
        return False


def run_crate_tests(crate, timeout=600):
    """Run `cargo test -p <crate>`. Returns (exit_code, tail_of_output)."""
    try:
        p = run(["cargo", "test", "-p", crate], timeout=timeout)
        tail = "\n".join(p.stdout.splitlines()[-40:])
        return p.returncode, tail
    except subprocess.TimeoutExpired:
        return None, f"cargo test -p {crate} timed out after {timeout}s"


def looks_like_compile_failure(output):
    return (
        "error[E" in output
        or "error: expected" in output
        or "could not compile" in output
        or ("error:" in output and "test result:" not in output)
    )


def evaluate_scenario(scenario, timeout):
    result = ScenarioResult(scenario)
    path = REPO / scenario.file_rel

    if not git_is_clean(scenario.file_rel):
        result.status = "error"
        result.detail = (
            f"{scenario.file_rel} already has uncommitted changes — refusing "
            "to mutate a dirty file (would clobber unrelated in-progress "
            "edits, and the revert step could not tell 'ours' from 'theirs')"
        )
        return result

    original_bytes = git_file_bytes(path)
    start = time.monotonic()
    try:
        err = apply_mutation(scenario)
        if err is not None:
            result.status = "error"
            result.detail = err
            return result

        rc, tail = run_crate_tests(scenario.crate, timeout=timeout)
        if rc is None:
            result.status = "inconclusive"
            result.detail = tail
            return result
        if rc == 0:
            result.status = "not-caught"
            result.detail = (
                "mutated build compiled and `cargo test -p "
                f"{scenario.crate}` PASSED (exit 0) — the existing test "
                "suite did NOT catch this fail-open mutation."
            )
            return result

        # Non-zero exit. Distinguish "tests ran and went red" (what we want)
        # from "the mutated code didn't even compile" (inconclusive per the
        # task's design requirements — it doesn't prove the test suite's
        # SEMANTIC judgment caught anything).
        if looks_like_compile_failure(tail):
            result.status = "inconclusive"
            result.detail = (
                "mutation caused a COMPILE failure, not a semantic test "
                f"failure — inconclusive for this crate's test coverage:\n{tail}"
            )
            return result

        result.status = "caught"
        result.red_confirmed = True
        result.detail = f"cargo test -p {scenario.crate} exited {rc} (RED):\n{tail}"
        return result
    except Exception as e:  # noqa: BLE001 - must still revert on any failure
        result.status = "error"
        result.detail = f"unexpected exception: {e!r}"
        return result
    finally:
        result.revert_confirmed = revert_mutation(scenario, original_bytes)
        result.elapsed = time.monotonic() - start


def print_result(result):
    s = result.scenario
    red = "yes" if result.red_confirmed else "no"
    revert = "yes" if result.revert_confirmed else "NO -- REVERT FAILED"
    print(f"--- {s.crate} :: {s.description}")
    print(f"    file: {s.file_rel}")
    print(f"    status: {result.status}  (RED confirmed: {red}; revert confirmed: {revert}; {result.elapsed:.1f}s)")
    if result.status != "caught":
        indented = "\n".join("      " + line for line in result.detail.splitlines())
        print(indented if indented.strip() else "      (no detail)")
    print()


def parse_args(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("--crate", choices=GATE_CRATES, help="run only this crate's scenario(s)")
    ap.add_argument("--keep-going", action="store_true", help="run all scenarios even after a NOT-CAUGHT/inconclusive result")
    ap.add_argument("--timeout", type=int, default=600, help="per-scenario cargo test timeout (seconds)")
    return ap.parse_args(argv)


def select_scenarios(crate_filter):
    """Scenarios matching `crate_filter` (or all, if None)."""
    return [s for s in SCENARIOS if crate_filter is None or s.crate == crate_filter]


def run_scenarios(scenarios, timeout, keep_going):
    """Evaluate each scenario in order, printing as it goes. Stops at the
    first non-"caught" result unless `keep_going` is set. Revert failures
    never stop the loop (every remaining scenario still gets its own
    mutate/revert cycle)."""
    results = []
    for s in scenarios:
        result = evaluate_scenario(s, timeout=timeout)
        results.append(result)
        print_result(result)
        if result.status != "caught" and not keep_going:
            break
    return results


def build_summary(results, total_scenarios):
    return {
        "ran": len(results),
        "total": total_scenarios,
        "caught": sum(1 for r in results if r.status == "caught"),
        "not_caught": [r for r in results if r.status == "not-caught"],
        "inconclusive": [r for r in results if r.status == "inconclusive"],
        "errored": [r for r in results if r.status == "error"],
        "revert_failed": [r for r in results if not r.revert_confirmed],
    }


def print_summary(summary):
    print("=== summary ===")
    print(f"  scenarios run: {summary['ran']}/{summary['total']}")
    print(f"  caught (RED confirmed): {summary['caught']}")
    print(f"  NOT caught (existing tests missed a real fail-open): {len(summary['not_caught'])}")
    print(f"  inconclusive (mutation didn't compile): {len(summary['inconclusive'])}")
    print(f"  errored (setup/revert problem): {len(summary['errored'])}")

    if summary["not_caught"]:
        print()
        print("FINDING: the following fail-open mutations were NOT caught by the")
        print("crate's existing test suite. This is a genuine gap, not a failure")
        print("of this script — do not paper over it by picking an easier mutation.")
        for r in summary["not_caught"]:
            print(f"  - {r.scenario.crate}: {r.scenario.file_rel} :: {r.scenario.description}")

    if summary["revert_failed"]:
        print()
        print("ERROR: at least one mutated file did NOT revert to its original byte "
              "content. Run `git status` / `git diff` and `git checkout --` manually.",
              file=sys.stderr)


def exit_code_for(summary):
    problem = summary["not_caught"] or summary["inconclusive"] or summary["errored"]
    return 1 if (problem or summary["revert_failed"]) else 0


def main(argv=None):
    args = parse_args(argv)
    scenarios = select_scenarios(args.crate)
    if not scenarios:
        print(f"no scenarios registered for crate {args.crate!r}", file=sys.stderr)
        return 1

    results = run_scenarios(scenarios, timeout=args.timeout, keep_going=args.keep_going)
    summary = build_summary(results, total_scenarios=len(scenarios))
    print_summary(summary)
    return exit_code_for(summary)


if __name__ == "__main__":
    sys.exit(main())
