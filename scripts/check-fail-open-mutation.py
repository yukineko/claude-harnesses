#!/usr/bin/env python3
"""Adversarial fail-open mutation harness for the 7 GATE crates.

The other fail-open defenses in this repo are STATIC: `check-fail-open.py`
greps for known swallow-shapes; code review reads doc comments and asserts
"this looks fail-closed". Neither of those actually PROVES that a fail-open
regression would be caught by the crate's own test suite — a static grep can
miss a shape it doesn't know about, and "I read the code and it looks right"
is exactly the unverified *judgment* the repo's top-level doctrine (see
CLAUDE.md §2, "判断は予測にすぎない") warns against substituting for an
observed fact.

This script is the missing DYNAMIC check: for each of the 7 GATE crates
(blastguard / propguard / specguard / stuckguard / mutategate / overwatch /
parallelguard) it

  1. copies the invoking tree into a SCRATCH temporary directory (every
     tracked and untracked-but-not-ignored path `git ls-files` lists, read
     from the working tree, so the copy holds exactly what an in-place
     `cargo test` would have compiled -- the whole workspace, so path
     dependencies such as harness-core resolve), then applies one concrete,
     mechanical fail-open mutation to the COPY of a real source file (a
     literal old-string -> new-string replacement, applied programmatically
     -- never "an LLM decides at check-time whether this would break
     something"). USER RULING 2026-11-14: the mutation and the test run
     happen in that scratch copy, and the invoking tree's tracked file is
     never written. The 2026-11-08 "no temp checkout" ruling is read
     narrowly as "no git worktree / second checkout", and this plain file
     copy is neither,
  2. runs `cargo test -p <crate> <scenario.tests>` with cwd = the scratch
     copy and CARGO_TARGET_DIR = <scratch>/target (never the invoking tree's
     target dir, so no mutant binary lands where hooks or rollouts look),
     and captures the real exit code. `scenario.tests` are the test-target
     selectors (`--lib`, `--test <name>`) that were OBSERVED to go RED under
     this mutation (and to pass without it) when the scenario was narrowed;
     the evidence is recorded next to each scenario. A scenario without
     declared selectors runs the whole `cargo test -p <crate>`.
  3. asserts the exit code is non-zero (i.e., the crate's OWN declared tests
     went RED because of the injected fail-open). A declared set that does
     NOT catch the mutation is NOT CAUGHT (exit 1), and a selector cargo
     rejects (`no test target named ...`) is inconclusive (exit 1), never a
     RED — narrowing never turns a miss into a pass, and
  4. in a `finally` block (so it runs even if step 2/3 raised or timed out):
     writes the original bytes back into the scratch copy, deletes the
     scratch directory (VERIFYING it is gone), and VERIFIES the invoking
     tree's target file is still byte-identical to its pre-run state. That
     file is only read, never written, so a mismatch (e.g. a concurrent
     edit) is reported as a failed cleanup and left untouched. A scratch copy
     that cannot be created, mutated, tested or removed is an ERROR (exit 1),
     never a pass.

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
      unexpected error occurred (including a scratch copy that could not be
      created, mutated, tested or removed). Details are always printed; the
      scratch copy is always removed first.

Usage:
  python3 scripts/check-fail-open-mutation.py
  python3 scripts/check-fail-open-mutation.py --crate mutategate   # one crate only
  python3 scripts/check-fail-open-mutation.py --keep-going         # don't stop at first NOT-CAUGHT

GATE_CRATES here is one more hardcoded copy of the canonical 7-crate list
(unavoidable: this is a standalone Python script, not Rust, so it cannot
`pub use harness_core::fleet::GATE_CRATES`). It is registered as an "exact"
source in `scripts/check-gate-crates-sync.py`'s SOURCES list so drift between
this copy and the canonical list is caught the same mechanical way every
other copy is.
"""
import argparse
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# The canonical 7 GATE crates (see crates/harness-core/src/fleet.rs and
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

    def __init__(self, crate, file_rel, description, old, new, tests=None):
        self.crate = crate
        self.file_rel = file_rel
        self.description = description
        self.old = old
        self.new = new
        # Cargo test-target selectors run for this scenario, e.g.
        # ("--test", "foo") or ("--lib",). None = the whole crate.
        self.tests = tuple(tests) if tests is not None else None

    def test_args(self):
        return list(self.tests) if self.tests else []


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
        # Evidence (rev 91046d26, 2026-11-14): mutated `--no-fail-fast` run
        # went RED in 72 targets, but 60 of them ALSO fail on the unmutated
        # tree when run alone, and an unmutated `cargo test -p blastguard
        # --no-fail-fast` failed 61 and 60 targets in two runs (committed RED repros) — so the
        # former whole-crate run was RED with or without the mutation and
        # proved nothing for this scenario. ask_hardening_invariant (not among
        # either list): mutated exit 101
        # ("test result: FAILED", 0.5s warm), unmutated exit 0.
        tests=("--test", "ask_hardening_invariant"),
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
        # Evidence (rev 91046d26, 2026-11-14): RED targets bin propguard,
        # backlog_b74955f4, hook_contract, integration,
        # repro_criteria_env_non_utf8, session_scoped_skip (all pass
        # unmutated). Fastest: backlog_b74955f4, mutated exit 101 (0.9s warm).
        tests=("--test", "backlog_b74955f4"),
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
        # Evidence (rev 91046d26, 2026-11-14): the only RED target is the
        # binary's unit tests (specguard has no tests/ dir): mutated exit 101
        # (5.4s warm), unmutated exit 0.
        tests=("--bin", "specguard"),
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
        # Evidence (rev 91046d26, 2026-11-14): RED targets bin stuckguard,
        # backlog_6108a7fa, faultinject_state, integration (all pass
        # unmutated). integration: mutated exit 101 (1.3s warm).
        tests=("--test", "integration"),
    ),
    Scenario(
        crate="parallelguard",
        file_rel="crates/parallelguard/src/model.rs",
        description=(
            "acquire(): hardcode the cap check to never fire, so a class that "
            "already holds `cap` live slots is admitted anyway — the width "
            "limit this gate exists to enforce, removed."
        ),
        old="        if live >= cap {\n"
        "            return Decision::Deny(deny_reason(class, live, cap));\n"
        "        }\n",
        new="        if false {\n"
        "            return Decision::Deny(deny_reason(class, live, cap));\n"
        "        }\n",
        # Evidence (rev 91046d26, 2026-11-14): RED targets bin parallelguard,
        # concurrency, hook_contract (all pass unmutated). concurrency:
        # mutated exit 101 (0.4s warm).
        tests=("--test", "concurrency"),
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
        # Evidence (rev 91046d26, 2026-11-14): RED targets lib, cli_contract,
        # overwatch_violation (all pass unmutated). lib: mutated exit 101
        # (0.1s warm).
        tests=("--lib",),
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
        # Evidence (rev 91046d26, 2026-11-14): RED targets lib, bin overwatch,
        # canary, cli_contract (all pass unmutated). canary: mutated exit 101
        # (0.5s warm).
        tests=("--test", "canary"),
    ),
]

assert {s.crate for s in SCENARIOS} <= set(GATE_CRATES)


class ScenarioResult:
    def __init__(self, scenario):
        self.scenario = scenario
        self.status = None  # "caught" | "not-caught" | "inconclusive" | "error"
        self.detail = ""
        self.red_confirmed = False
        # "Cleanup confirmed" (the name predates the 2026-11-14 scratch-copy
        # ruling): scratch copy restored and removed, and the invoking
        # tree's target file byte-identical to its pre-run bytes. Default
        # True ("nothing to clean up"): only the paths that reach the scratch
        # step set it to the real outcome. The pre-flight refusals (dirty
        # file, marker) return before any copy exists, so they must not be
        # reported as "CLEANUP FAILED". The pre-existing (unrelated) dirty
        # state is correctly left untouched.
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
    """True iff `git status --porcelain` reports no changes for this path.

    A failed git call is NOT clean (CLAUDE.md section 3), so the caller
    refuses."""
    p = run(["git", "status", "--porcelain", "--", path_rel])
    return p.returncode == 0 and p.stdout.strip() == ""


def git_file_bytes(path):
    return path.read_bytes()


class ScratchError(Exception):
    """The scratch copy could not be created or used. evaluate_scenario turns
    it into status "error" (never a pass)."""


# The scratch copy that the current scenario's `cargo test` runs in.
# evaluate_scenario sets it for one scenario and clears it in its `finally`.
# It is module state, not an argument, so the call keeps the shape
# `run_crate_tests(crate, timeout=..., test_args=...)` that the unit tests'
# stubs replace. run_crate_tests refuses to run while it is unset, and never
# falls back to the invoking tree.
_ACTIVE_SCRATCH = None


def make_scratch_copy():
    """Copy the invoking tree (REPO) into a new temporary directory and return
    its path. Raises ScratchError on any failure, after removing the partial
    copy.

    Copied: every path `git ls-files -z --cached --others --exclude-standard`
    lists, read from the WORKING TREE, so the copy holds what an in-place
    `cargo test` would have compiled. Ignored paths such as target/ are not
    copied. Symlinks are copied as symlinks. Listed paths that no longer exist
    on disk are skipped, since an in-place cargo would not see them either.
    This is a plain file copy: no git worktree, no checkout, no .git
    (2026-11-08 ruling, read narrowly per the 2026-11-14 ruling)."""
    p = subprocess.run(
        ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
        cwd=str(REPO), stdout=subprocess.PIPE, stderr=subprocess.PIPE,
    )
    if p.returncode != 0:
        raise ScratchError(
            f"git ls-files failed (exit {p.returncode}) in {REPO}: "
            f"{p.stderr.decode('utf-8', 'replace').strip()}"
        )
    rels = [r.decode("utf-8", "surrogateescape") for r in p.stdout.split(b"\0") if r]
    if not rels:
        raise ScratchError(f"git ls-files listed no files in {REPO}; nothing to copy")
    scratch = Path(tempfile.mkdtemp(prefix="fail-open-mutation-")).resolve()
    try:
        for rel in rels:
            src = REPO / rel
            dst = scratch / rel
            if src.is_symlink():
                dst.parent.mkdir(parents=True, exist_ok=True)
                os.symlink(os.readlink(src), dst)
            elif src.is_file():
                dst.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(src, dst)
            # else: deleted in the working tree, or a gitlink dir -- skipped.
    except OSError as e:
        remove_scratch_copy(scratch)
        raise ScratchError(f"cannot copy {REPO} into scratch {scratch}: {e}") from e
    return scratch


def remove_scratch_copy(scratch):
    """Delete the scratch copy. True iff it is confirmed gone."""
    shutil.rmtree(scratch, ignore_errors=True)
    return not os.path.lexists(scratch)


def apply_mutation(scenario, root=None):
    """Apply the mutation to `root / scenario.file_rel`. Returns None on
    success, or an error string.

    evaluate_scenario always passes the scratch copy as `root`. The default
    (the module-level REPO, resolved at call time) is there only for the unit
    tests that drive this helper against their own throwaway repo. The script
    never calls it on the invoking tree (2026-11-14 ruling).

    Requires `old` to appear EXACTLY once (ambiguity is treated as a hard
    stop, never a guess at which occurrence to mutate). A symlinked target is
    refused, because writing through it would write wherever it points.
    """
    path = (root if root is not None else REPO) / scenario.file_rel
    if path.is_symlink():
        return f"{scenario.file_rel} is a symlink — refusing to write through it"
    text = path.read_text(encoding="utf-8")
    count = text.count(scenario.old)
    if count != 1:
        return (
            f"expected exactly 1 occurrence of the mutation anchor in "
            f"{scenario.file_rel}, found {count} — refusing to guess"
        )
    mutated = text.replace(scenario.old, scenario.new, 1)
    path.write_text(mutated, encoding="utf-8")
    if path.read_text(encoding="utf-8") != mutated:
        return f"{scenario.file_rel}: the mutated content did not read back as written"
    return None


MARKER_NAME = "fail-open-mutation.active"
SEQ_NAME = "fail-open-mutation.seq"


def mutation_git_dir():
    """This worktree's git dir (absolute), or None when git cannot say."""
    p = run(["git", "rev-parse", "--absolute-git-dir"])
    gd = p.stdout.strip() if p.returncode == 0 else ""
    return Path(gd) if gd else None


def begin_mutation_marker():
    """Announce a mutation run to scripts/rollout_source_guard.py (backlog
    ff8cebd0): bump <git-dir>/fail-open-mutation.seq and create
    <git-dir>/fail-open-mutation.active BEFORE the mutant is written. A
    rollout refuses while .active exists, and refuses when .seq moved during
    its run.

    History: the marker was added when the mutation was written IN PLACE into
    the invoking tree, where a concurrent rollout could build and deploy the
    mutant. Since the 2026-11-14 ruling the mutant exists only in a scratch
    copy outside the tree and builds into the scratch's own target dir, so a
    rollout of this tree cannot pick it up. The marker stays as a conservative
    second barrier, and failing to write it still means refusing to mutate.
    Returns None on success, or an error string."""
    gd = mutation_git_dir()
    if gd is None:
        return "cannot locate the git dir to write the mutation marker"
    seq_path = gd / SEQ_NAME
    try:
        cur = 0
        if seq_path.exists():
            raw = seq_path.read_text(encoding="utf-8").strip()
            if not raw.isdigit():
                return f"{seq_path} does not hold an integer ({raw!r})"
            cur = int(raw)
        tmp = gd / (SEQ_NAME + ".tmp")
        tmp.write_text(f"{cur + 1}\n", encoding="utf-8")
        tmp.replace(seq_path)
        (gd / MARKER_NAME).write_text("active\n", encoding="utf-8")
    except OSError as e:
        return f"cannot write the mutation marker in {gd}: {e}"
    return None


def end_mutation_marker():
    """Remove .active. Called ONLY after the cleanup is confirmed (scratch copy
    gone, the invoking tree's target file byte-identical). An unconfirmed
    cleanup leaves it in place, so rollouts keep refusing."""
    gd = mutation_git_dir()
    if gd is None:
        return False
    try:
        (gd / MARKER_NAME).unlink()
    except FileNotFoundError:
        pass
    except OSError:
        return False
    return True


def revert_mutation(scenario, original_bytes, root=None):
    """Write `original_bytes` back to `root / scenario.file_rel`, then verify
    byte-identity. Returns True iff that is confirmed.

    evaluate_scenario calls it on the SCRATCH copy, from its `finally`,
    before deleting the copy, so a scratch dir that cannot be fully removed
    does not keep a mutant. The default root (REPO, resolved at call time) is
    there only for the unit tests that drive this helper against their own
    throwaway repo. The script never writes the invoking tree (2026-11-14
    ruling).
    """
    path = (root if root is not None else REPO) / scenario.file_rel
    try:
        if path.is_symlink():
            return False
        path.write_bytes(original_bytes)
        return path.read_bytes() == original_bytes
    except OSError:
        return False


def invoking_tree_unchanged(scenario, original_bytes):
    """Read-only check that the invoking tree's target file still holds its
    pre-run bytes. It never writes the file."""
    try:
        return (REPO / scenario.file_rel).read_bytes() == original_bytes
    except OSError:
        return False


def run_crate_tests(crate, timeout=600, test_args=None, cwd=None):
    """Run `cargo test -p <crate> [test_args...]` in the scratch copy (`cwd`,
    which defaults to the active scratch that evaluate_scenario sets), with
    CARGO_TARGET_DIR=<scratch>/target. Returns (exit_code, tail_of_output).
    exit_code is None on timeout, and the whole process group is killed.

    Raises ScratchError when there is no scratch copy to run in. It never
    falls back to the invoking tree."""
    root = cwd if cwd is not None else _ACTIVE_SCRATCH
    if root is None:
        raise ScratchError(
            "no scratch copy to run cargo in — refusing to run the mutated "
            "tests in the invoking tree"
        )
    root = Path(root)
    cmd = ["cargo", "test", "-p", crate] + list(test_args or [])
    env = dict(os.environ, CARGO_TARGET_DIR=str(root / "target"))
    proc = subprocess.Popen(
        cmd, cwd=str(root), env=env, stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT, text=True, start_new_session=True,
    )
    try:
        out, _ = proc.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except OSError:
            proc.kill()
        proc.communicate()
        return None, f"{' '.join(cmd)} timed out after {timeout}s"
    tail = "\n".join((out or "").splitlines()[-40:])
    return proc.returncode, tail


def looks_like_compile_failure(output):
    return (
        "error[E" in output
        or "error: expected" in output
        or "could not compile" in output
        or ("error:" in output and "test result:" not in output)
    )


def evaluate_scenario(scenario, timeout):
    global _ACTIVE_SCRATCH
    result = ScenarioResult(scenario)
    path = REPO / scenario.file_rel

    if not git_is_clean(scenario.file_rel):
        result.status = "error"
        result.detail = (
            f"{scenario.file_rel} already has uncommitted changes (or git could "
            "not tell) — refusing to mutate a dirty file (the scratch copy "
            "would test unrelated in-progress edits, and the post-run "
            "byte-identity check could not tell 'ours' from 'theirs')"
        )
        return result

    original_bytes = git_file_bytes(path)
    marker_err = begin_mutation_marker()
    if marker_err is not None:
        result.status = "error"
        result.detail = (
            f"{marker_err} -- refusing to mutate: the marker tells a "
            "concurrent rollout-plugins.sh that a mutation run is in flight "
            "(backlog ff8cebd0)"
        )
        return result
    start = time.monotonic()
    scratch = None
    try:
        # The mutant lives ONLY in this scratch copy (USER RULING
        # 2026-11-14). REPO / file_rel is read, never written.
        scratch = make_scratch_copy()
        copy_path = scratch / scenario.file_rel
        if copy_path.is_symlink() or not copy_path.is_file() or copy_path.read_bytes() != original_bytes:
            result.status = "error"
            result.detail = (
                f"the scratch copy of {scenario.file_rel} is missing, a symlink, "
                "or differs from the invoking tree's file — refusing to test it"
            )
            return result
        err = apply_mutation(scenario, root=scratch)
        if err is not None:
            result.status = "error"
            result.detail = err
            return result

        _ACTIVE_SCRATCH = scratch
        shown = " ".join(["cargo test -p", scenario.crate] + scenario.test_args())
        rc, tail = run_crate_tests(scenario.crate, timeout=timeout, test_args=scenario.test_args())
        if rc is None:
            result.status = "inconclusive"
            result.detail = tail
            return result
        if rc == 0:
            result.status = "not-caught"
            result.detail = (
                f"mutated build compiled and `{shown}` PASSED (exit 0) — the "
                "declared tests did NOT catch this fail-open mutation."
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
        result.detail = f"{shown} exited {rc} (RED):\n{tail}"
        return result
    except Exception as e:  # noqa: BLE001 - must still clean up on any failure
        result.status = "error"
        result.detail = f"unexpected exception: {e!r}"
        return result
    finally:
        _ACTIVE_SCRATCH = None
        problems = []
        if scratch is not None:
            # Restore the copy first, so a scratch dir that cannot be fully
            # removed does not keep a mutant around.
            copy_path = scratch / scenario.file_rel
            if os.path.lexists(copy_path) and not revert_mutation(scenario, original_bytes, root=scratch):
                problems.append(f"could not restore the scratch copy of {scenario.file_rel}")
            if not remove_scratch_copy(scratch):
                problems.append(f"could not remove the scratch copy {scratch}")
        if not invoking_tree_unchanged(scenario, original_bytes):
            problems.append(
                f"{scenario.file_rel} in the invoking tree no longer matches its "
                "pre-run bytes (this script never writes it; left untouched)"
            )
        # `revert_confirmed` now means "cleanup confirmed": scratch restored
        # and gone, and the invoking tree's file byte-identical.
        result.revert_confirmed = not problems
        if problems:
            result.status = "error"
            result.red_confirmed = False
            result.detail = (result.detail + "\n" if result.detail else "") + "; ".join(problems)
        if result.revert_confirmed and not end_mutation_marker():
            result.status = "error"
            result.detail = (
                f"cleaned up, but could not remove the {MARKER_NAME} marker in "
                "the git dir -- rollouts refuse until it is removed by hand"
            )
        result.elapsed = time.monotonic() - start


def print_result(result):
    s = result.scenario
    red = "yes" if result.red_confirmed else "no"
    revert = "yes" if result.revert_confirmed else "NO -- CLEANUP FAILED"
    print(f"--- {s.crate} :: {s.description}")
    print(f"    file: {s.file_rel}")
    print(f"    status: {result.status}  (RED confirmed: {red}; scratch cleanup confirmed: {revert}; {result.elapsed:.1f}s)")
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
    first non-"caught" result unless `keep_going` is set. Cleanup failures
    never stop the loop (every remaining scenario still gets its own
    copy/mutate/test/remove cycle)."""
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
    print(f"  errored (setup/scratch-copy/cleanup problem): {len(summary['errored'])}")

    if summary["not_caught"]:
        print()
        print("FINDING: the following fail-open mutations were NOT caught by the")
        print("crate's existing test suite. This is a genuine gap, not a failure")
        print("of this script — do not paper over it by picking an easier mutation.")
        for r in summary["not_caught"]:
            print(f"  - {r.scenario.crate}: {r.scenario.file_rel} :: {r.scenario.description}")

    if summary["revert_failed"]:
        print()
        print("ERROR: at least one scenario's cleanup is unconfirmed: a scratch copy "
              "was not restored/removed, or the invoking tree's target file no longer "
              "matches its pre-run bytes (this script never writes it). See the "
              "details above and run `git status` / `git diff`.",
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
