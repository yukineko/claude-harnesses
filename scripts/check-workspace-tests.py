#!/usr/bin/env python3
"""Run the workspace's tests and return a verdict — both bodies, one gate.

Two independent test bodies exist in this repository and, until this script, NO
git hook ran either of them (measured 2026-09-10 at d415d5ea:
`grep -rn 'cargo test\\|unittest' .githooks/` returns zero hits; the only
workspace-wide command any hook runs is `cargo check --workspace --all-targets`
at .githooks/pre-push:375, which compiles but never executes a test).

What that gap cost, measured in this worktree on 2026-09-10:

  `cargo test --workspace --no-fail-fast` (stdin closed — see STDIN below):
  232 targets, 4937 passing tests, and 4 FAILING tests in 3 targets. 345.2s
  from an empty target dir, 152.8s warm. Two of the four are in blastguard, a
  gate DEPLOYED on this machine. Nobody had seen any of them.

  Verbatim, in the "<target> :: <test>" form this script prints (the crate each
  target belongs to is noted in brackets — cargo does not put the package on a
  `Running` line, so the script does not invent one):

    project_scope (tests/project_scope.rs) ::
        a_pinned_store_dir_still_scopes_by_project            [crates/backlog]
    project_scope (tests/project_scope.rs) ::
        a_pinned_store_dir_still_works_outside_any_repo       [crates/backlog]
    blastguard (unittests src/lib.rs) ::
        detect::tests::bg2_fd_dup_and_safe_targets_stay_allowed
    scoped_destructive (tests/scoped_destructive.rs) ::
        relaxed_truncating_forms_inside_the_project        [crates/blastguard]

  Those 4 appeared in all three full runs. A FIFTH appeared in the third run
  only — the first end-to-end run of this script itself:

    condukt (unittests src/main.rs) ::
        diffrisk_record::tests::every_invocation_is_journaled_even_when_nothing_is_recorded

  Re-run alone it passes (`ok. 1 passed`), so it is INTERMITTENT, and the honest
  statement is "observed failing in 1 of 3 workspace runs at this tree", not
  "flaky" (that would be a diagnosis, and none was made). Filed separately. It
  is recorded here because it is also the first evidence that this gate catches
  reds the two hand-run measurements missed — which is the entire argument for
  having it. An intermittent red is exactly the kind a human running the suite
  by hand once decides not to believe.

  scripts/ ships 23 unittest suites, 905 tests, 193.4s serial — all green as of
  the same measurement. (Backlog 65bfb279's "~60s" estimate is stale; the number
  above is the re-measurement.) That body has been dark twice: reds sat unseen
  for 11 and 33 days.

  So a full run of this gate costs about 152.8 + 193.4 = 346s warm — call it
  six minutes, not the ~156s that was estimated before either half was
  re-measured. That number is written here rather than rounded down because the
  cost is the argument for where this gate is wired, and a gate whose stated
  cost is half its real one gets moved for the wrong reason later.

A red nobody runs is not a detection (CLAUDE.md 1). This script is the runner.

STDIN — WHY EVERY CHILD GETS /dev/null
--------------------------------------
`crates/harness-core/src/hook.rs` has a test that calls `read_stdin_if_piped()`.
Its own comment claims verbatim "It must not block on an interactive read"; the
observed behaviour contradicts that (a CLAUDE.md 4 prose-vs-behaviour defect,
filed separately — this gate must be correct WHILE it exists). Controlled
measurement, 2026-09-10:

    cargo test -p harness-core --lib <that test> -- --exact < /dev/null
      -> ok, finished in 0.00s
    (sleep 25) | cargo test -p harness-core --lib <that test> -- --exact
      -> ok, but finished in 24.81s

The runtime is exactly the lifetime of stdin's writer. With a writer that never
closes it never returns: one such run sat in `semaphore_wait` for 15h42m, wrote
a PARTIAL result file, and that partial file was briefly mistaken for a
completed run. A body that never finished is not a body that passed, and it must
not be readable as one.

git hands a pre-push hook its ref list ON STDIN. A gate that shells out to
`cargo test --workspace` from inside pre-push while letting the child inherit
that stdin therefore does two bad things at once: the test binary can consume
the bytes the hook still needs, and the push can hang forever with no
diagnostic. So EVERY child launched here — cargo and python alike — is given
`stdin=/dev/null`, unconditionally. There is no switch to turn that off. The
345.2s/152.8s figures above were both measured that way, and both runs
terminated — which is itself the evidence that closing stdin removes this hang.

DEADLINES — WHY A HANG IS A BLOCK
---------------------------------
Closing stdin removes the known hang, but "the child never came back" must stay
representable, because the next such test will not announce itself. Every child
runs under a wall-clock deadline (`timeout(1)` is not available on this machine,
so it is enforced in Python). On expiry the child is killed by PROCESS GROUP —
each child is launched in its own session, because killing only the `cargo`
parent leaves the test binaries orphaned (one such orphan had to be killed by
hand after 15 hours on 2026-09-10). Expiry is its own exit class: the message
names the deadline and the last target that had been announced. It is never a
pass and never a silent skip.

WHAT IS JUDGED: the WORKING TREE at os.getcwd(), not the commit being pushed.
This differs on purpose from the type-check block in .githooks/pre-push, which
checks out each pushed sha into a throwaway worktree precisely so a dirty tree
cannot yield a verdict about content that is not being pushed. The trade is
stated rather than hidden: a fresh detached worktree means a COLD target dir,
and the measurement above puts that at 345.2s versus 152.8s warm. So the honest
reading of a green from this script is "the tree on disk passes its tests", not
"the pushed commit passes its tests". If the tree is clean those coincide; if it
is dirty they do not.

EXIT CODES (distinct per failure CLASS, following the shape of
scripts/check-plugin-rollout.py: a caller that conflates classes sends the
reader to the wrong fix). EVERY non-zero code BLOCKS. There is no advisory
class here, no bypass flag and no environment escape hatch — if a commit truly
must get out ahead of a red, the repo already has exactly one mechanism for that
(the bypass ledger, scripts/gate-bypass.py), and inventing a second one here is
the thing CLAUDE.md 4 forbids.

  0 — OK. Both bodies ran to completion and every test in both passed.
  1 — CARGO_FAILURE. `cargo test --workspace --no-fail-fast` ran, its output
      parsed, the count of failures it reported matched the names extractable
      from it, and at least one test FAILED. The failing test NAMES and their
      TARGET names are printed — counts alone are what made these invisible.
  2 — PYTHON_FAILURE. At least one scripts/test_*.py suite ran, parsed, and
      reported a FAIL or ERROR. The suite name and every failing test name are
      printed.
  3 — CARGO_UNDETERMINED. The cargo body yielded no verdict: cargo is not
      resolvable, the child could not be launched, it was killed by a signal, it
      exited non-zero without ever printing a `test result:` line (a build or
      link error — the workspace never got as far as running tests), its exit
      status contradicts its output, or the failure count it reported does not
      match the failing names extractable from it. A checker that crashed is NOT
      a checker that passed. Never reported as "0 failures".
  4 — PYTHON_UNDETERMINED. The python body yielded no verdict: python3 is not
      resolvable, suite DISCOVERY failed or found zero suites (an empty list is
      not "nothing to run" — this repo has 23), a suite failed to IMPORT, was
      killed by a signal, reported `Ran 0 tests`, or its exit status contradicts
      its summary line.
  5 — ENVIRONMENT. The run could not be set up at all: cwd is not a directory
      that looks like this repository (crates/ missing, OR scripts/ missing —
      EITHER ONE is enough, they are not required together), or an argument was
      passed (this script takes none — an unrecognized argument is refused
      rather than ignored, per the no-bypass note above). Ranked highest because
      nothing below it was computed. A repo with crates/ but no scripts/ must
      NOT fall through to the python body and surface as a discovery problem:
      the remedy for "you are not in the repo" is not the remedy for "the suites
      are gone".
  6 — DEADLINE. A child exceeded its wall-clock deadline and was killed. Its own
      code because its remedy is neither "fix a test" nor "install a tool":
      something is HANGING, and the reader needs to be sent to the hang, with
      the deadline and the last announced target named.

RANKING when several classes fire at once: 5 > 6 > 3 > 4 > 1 > 2. Undetermined
outranks failure deliberately — "3 tests failed" printed over a body that never
ran is a lie of exactly the kind this gate exists to stop. The exit code carries
the highest-ranked class, but EVERY class that fired is printed in full first,
so nothing is suppressed by the ranking.

Design for testability (scripts/test_check_workspace_tests.py): parsing and
verdict are pure functions over captured text, and every process launch goes
through a single injected `runner` callable. `main()` reaches `resolve_cargo` /
`resolve_python` through the MODULE GLOBALS on purpose, so a test can rebind
them the way scripts/test_check_plugin_rollout.py rebinds SOURCE_CHANGED_SINCE;
do not turn those calls into local aliases or from-imports, or that injection
point silently disappears. The suite therefore exercises every class above
deterministically and never runs the real workspace suite.
"""
import glob
import os
import re
import shutil
import signal
import subprocess
import sys
import time
from collections import namedtuple

RC_OK = 0
RC_CARGO_FAILURE = 1
RC_PYTHON_FAILURE = 2
RC_CARGO_UNDETERMINED = 3
RC_PYTHON_UNDETERMINED = 4
RC_ENVIRONMENT = 5
RC_DEADLINE = 6

REPO = os.getcwd()

CARGO_ARGV = ("test", "--workspace", "--no-fail-fast")

# Wall-clock deadlines, seconds. Deliberately constants and not read from the
# environment: a variable that lengthens a deadline is a variable that disables
# this class, and that is the second escape hatch CLAUDE.md 4 forbids.
#
# Sized from measurement, not taste. The python body's slowest suite is
# test_gate_bypass at 67.7s (2026-09-10, this worktree), so 600s is ~9x headroom
# per suite. The cargo body measured 345.2s cold / 152.8s warm the same day, so
# 1800s is ~5x the cold figure. A deadline that fires on a slow-but-healthy
# machine
# would train readers to route around this gate; a deadline that never fires is
# not a deadline. These are sized to be reachable only by a hang.
CARGO_DEADLINE = 1800.0
PYTHON_SUITE_DEADLINE = 600.0

# Printed instead of guessing which target a stray failure line belongs to.
UNKNOWN_TARGET = "<unknown target>"

# A launched child. `rc` follows subprocess's POSIX convention: negative means
# the child was killed by signal -rc. `launch_error` is non-None only when the
# process could not be started at all, and in that case rc/out say nothing.
# `timed_out` is True only when THIS module killed the child for exceeding its
# deadline — distinguishable from a signal the child got for any other reason.
Completed = namedtuple("Completed", "argv rc out launch_error timed_out deadline")

# Exactly one of `path` / `problem` is non-None. Never both None: "could not
# determine where cargo is" has to be representable, or it collapses into the
# same value as "cargo is right here".
Resolved = namedtuple("Resolved", "path problem")

# `failures` are named, reproducible reds. `undetermined` are the things this
# script could not decide — kept in their OWN list rather than folded into
# failures or (worse) dropped, because their remedy differs and because an empty
# `failures` next to a non-empty `undetermined` must never print as clean.
# `deadline` is split out of `undetermined` for the same reason again: its
# remedy is "find the hang".
#
# `checked` is the SIZE OF THE POPULATION this report covers, and exists so a
# green line can never be printed over a population nobody counted: for the
# cargo body it is the number of targets that announced themselves, for the
# python body the number of suites discovered and run. It is 0 whenever the body
# never got far enough to enumerate anything (unresolvable tool, failed
# discovery) — and a 0 next to a green line is therefore a contradiction a
# reader can see, not a silent pass.
Report = namedtuple("Report", "failures undetermined deadline checked")

# What a single `cargo test` invocation's text said, before any verdict.
CargoParse = namedtuple(
    "CargoParse", "targets failed_names reported_failed result_lines"
)

# What a single unittest suite's text said, before any verdict.
UnittestParse = namedtuple("UnittestParse", "ran outcome failed_names import_error")


def _signal_name(num):
    try:
        return signal.Signals(num).name
    except (ValueError, AttributeError):
        return "unrecognized signal"


def _kill_process_group(proc):
    """SIGTERM then SIGKILL the child's whole process GROUP.

    Killing only the `cargo` parent leaves its test binaries running: one such
    orphan had to be found and killed by hand after 15 hours on 2026-09-10.
    run_process starts every child in its own session precisely so this can
    address the group without touching the hook that launched us.
    """
    try:
        pgid = os.getpgid(proc.pid)
    except OSError:
        pgid = None
    for sig in (signal.SIGTERM, signal.SIGKILL):
        try:
            if pgid is not None:
                os.killpg(pgid, sig)
            else:
                proc.send_signal(sig)
        except OSError:
            pass
        try:
            proc.wait(timeout=5)
            return
        except (subprocess.TimeoutExpired, OSError):
            continue


def run_process(argv, cwd, deadline, env=None):
    """Launch `argv` under `deadline` seconds and return a Completed.

    Three properties this function exists to guarantee, none of them optional:

    * stdin is /dev/null. See the STDIN section of the module docstring — a
      child that inherits a pre-push hook's stdin can eat the ref list the hook
      still needs, or block on it forever.
    * stderr is merged into stdout. cargo prints its `Running <target> (...)`
      banners on stderr while libtest prints `test <name> ... FAILED` on stdout,
      and their INTERLEAVING is the only thing tying a failing name to a target.
    * the child gets its own session, so the deadline can kill the whole process
      GROUP. Killing just `cargo` leaves the test binaries orphaned.

    Never raises for a child that fails; a child that could not be launched
    comes back as launch_error, which callers resolve to UNDETERMINED.
    """
    argv = tuple(argv)
    try:
        proc = subprocess.Popen(
            list(argv),
            cwd=cwd,
            env=env,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            start_new_session=True,
        )
    except (OSError, ValueError) as exc:
        return Completed(
            argv, None, "", "%s: %s" % (type(exc).__name__, exc), False, deadline
        )

    timed_out = False
    try:
        out, _ = proc.communicate(timeout=deadline)
    except subprocess.TimeoutExpired:
        timed_out = True
        _kill_process_group(proc)
        try:
            out, _ = proc.communicate(timeout=30)
        except (subprocess.TimeoutExpired, OSError):
            out = b""
    if isinstance(out, bytes):
        out = out.decode("utf-8", "replace")
    return Completed(argv, proc.returncode, out or "", None, timed_out, deadline)


def resolve_cargo(repo=None, environ=None, which=None, isfile=None):
    """Locate a runnable cargo, or say precisely why one could not be found.

    Mirrors what .githooks/pre-push does in sh (`[ -f "$HOME/.cargo/env" ] && .
    "$HOME/.cargo/env"`, then `command -v cargo`), because a git hook does not
    inherit the user's interactive PATH. Python cannot `source` a shell file, so
    this resolves the same two places directly: PATH first, then
    $HOME/.cargo/bin/cargo.

    `repo` is accepted so callers can pass their repository root uniformly
    alongside resolve_python(), and is genuinely UNUSED by the resolution today
    — the rust toolchain here is a property of $HOME and PATH, not of the
    checkout. It is documented as unused rather than quietly consulted, because
    a parameter that might or might not affect a gate's answer is worse than one
    that provably does not.

    The not-found shapes get DIFFERENT messages — "no rustup env file and no
    cargo anywhere" and "the rustup env file exists but its bin/cargo does not"
    send the reader to different remedies, so collapsing them into one string
    would be a small lie.
    """
    environ = os.environ if environ is None else environ
    which = shutil.which if which is None else which
    isfile = os.path.isfile if isfile is None else isfile

    found = which("cargo")
    if found:
        return Resolved(found, None)

    home = environ.get("HOME") or ""
    env_file = os.path.join(home, ".cargo", "env") if home else "$HOME/.cargo/env"
    candidate = os.path.join(home, ".cargo", "bin", "cargo") if home else ""

    if candidate and isfile(candidate):
        return Resolved(candidate, None)
    if home and isfile(env_file):
        return Resolved(
            None,
            "cargo is not on PATH, and although %s exists there is no %s to "
            "run. The rustup toolchain looks half-installed, so the workspace "
            "tests could NOT be run. That is 'unknown', not 'they passed' — "
            "reinstall the toolchain (rustup toolchain install stable) and "
            "re-run." % (env_file, candidate),
        )
    return Resolved(
        None,
        "cargo is not on PATH and there is no %s to fall back to, so the "
        "workspace tests could NOT be run. That is 'unknown', not 'they "
        "passed'. Install the rustup toolchain, or make it visible to git hooks "
        "the way .githooks/pre-push does (. \"$HOME/.cargo/env\")." % env_file,
    )


def resolve_python(which=None):
    """Locate a runnable python3 interpreter, or say why not.

    sys.executable is preferred (by definition runnable, and it is the
    interpreter already running this script); PATH is the fallback for the case
    where sys.executable is unset or empty.
    """
    which = shutil.which if which is None else which
    exe = sys.executable
    if exe:
        return Resolved(exe, None)
    for name in ("python3", "python"):
        found = which(name)
        if found:
            return Resolved(found, None)
    return Resolved(
        None,
        "no python3 interpreter could be resolved (sys.executable is empty and "
        "neither python3 nor python is on PATH), so the scripts/test_*.py "
        "suites could NOT be run. That is not the same as running them and "
        "finding nothing wrong.",
    )


_RUNNING_RE = re.compile(r"^\s*Running\s+(?P<desc>.+?)\s+\((?P<bin>[^()]*)\)\s*$")
_DOCTEST_RE = re.compile(r"^\s*Doc-tests\s+(?P<crate>\S+)\s*$")
_TESTLINE_RE = re.compile(r"^test\s+(?P<name>.+?)\s+\.\.\.\s+(?P<verdict>\S+)")
_RESULT_RE = re.compile(
    r"^test result:\s+(?P<verdict>\S+?)\.\s+(?P<passed>\d+)\s+passed;\s+"
    r"(?P<failed>\d+)\s+failed;"
)
_HASH_SUFFIX_RE = re.compile(r"-[0-9A-Fa-f]{6,}$")


def _crate_from_binary(binpath):
    base = os.path.basename((binpath or "").strip())
    if base.endswith(".exe"):
        base = base[: -len(".exe")]
    stripped = _HASH_SUFFIX_RE.sub("", base)
    return stripped or "<unknown crate>"


def parse_cargo_test_output(text):
    """Extract targets, failing test names and reported counts. Pure.

    Returns CargoParse:
      targets        — tuple of target labels in announcement order. The label
                       is "<binary stem, hash suffix removed> (<what cargo
                       called it>)", which renders cargo's three banner shapes
                       as:
                         Running unittests src/lib.rs (…/deps/blastguard-9a2f1c)
                           -> "blastguard (unittests src/lib.rs)"
                         Running tests/scoped_destructive.rs
                                 (…/deps/scoped_destructive-1122ee)
                           -> "scoped_destructive (tests/scoped_destructive.rs)"
                         Doc-tests blastguard
                           -> "blastguard (doc-tests)"
                       For an integration test the stem is the TEST TARGET's
                       name, not the package's — cargo does not put the package
                       on the Running line, and inventing one would be a guess.
                       Doc-test banners ARE counted as targets: they run tests
                       and emit their own `test result:` line (usually
                       `0 passed; 0 failed`), so omitting them would make
                       `targets` disagree with `result_lines`.
      failed_names   — tuple of (target_label, test_name). target_label is the
                       most recently announced target, or UNKNOWN_TARGET when a
                       failure line appears before any banner (itself a signal,
                       not something to attribute to whatever target is handy).
                       Only lines that START a line are considered, so libtest's
                       indented `failures:` recap block cannot double-count.
      reported_failed— sum of the `N failed` fields across every `test result:`
                       line.
      result_lines   — how many `test result:` lines were seen at all. Zero of
                       them next to a non-zero exit is a build error, not a
                       green.
    """
    targets = []
    failed_names = []
    reported_failed = 0
    result_lines = 0
    current = None

    for raw in (text or "").splitlines():
        line = raw.rstrip()

        m = _RUNNING_RE.match(line)
        if m:
            current = "%s (%s)" % (
                _crate_from_binary(m.group("bin")),
                m.group("desc").strip(),
            )
            targets.append(current)
            continue

        m = _DOCTEST_RE.match(line)
        if m:
            current = "%s (doc-tests)" % m.group("crate")
            targets.append(current)
            continue

        m = _RESULT_RE.match(line.strip())
        if m:
            result_lines += 1
            reported_failed += int(m.group("failed"))
            continue

        m = _TESTLINE_RE.match(line)
        if m and m.group("verdict") == "FAILED":
            failed_names.append((current or UNKNOWN_TARGET, m.group("name").strip()))

    return CargoParse(
        tuple(targets), tuple(failed_names), reported_failed, result_lines
    )


def cargo_verdict(completed, parse):
    """Fold a Completed plus its CargoParse into a Report.

    The EXIT STATUS is part of the verdict, not a footnote to the text
    (CLAUDE.md 3). In order, and the order matters where inputs overlap:

      - timed_out                            -> deadline, naming the limit and
                                                the last announced target.
      - launch_error                         -> undetermined, named.
      - rc is None                           -> undetermined: no status at all.
      - rc < 0 (killed by signal)            -> undetermined, naming the signal.
      - rc != 0 with result_lines == 0       -> undetermined: NEVER RAN TESTS
                                                (build or link error). Checked
                                                BEFORE the contradiction branch
                                                below, which a build error also
                                                satisfies — "it never got to the
                                                tests" is the more specific and
                                                more actionable of the two.
      - rc == 0 but the text reports failures -> undetermined: contradiction.
      - rc != 0 and the text reports no failure at all -> undetermined:
                                                contradiction.
      - reported_failed != len(failed_names) -> undetermined: count and names
                                                disagree, so the printed list
                                                would be incomplete.
      - any failed_names left                -> FAILURES, one entry per failing
                                                test, formatted
                                                "<target> :: <test>". This is
                                                the branch RC_CARGO_FAILURE
                                                exists for.
      - rc == 0 with result_lines == 0       -> undetermined: nothing ran, so
                                                there is nothing to call green.
      - otherwise                            -> clean.
    """
    checked = len(parse.targets)

    if completed.timed_out:
        last = (
            parse.targets[-1]
            if parse.targets
            else "none (no target had been announced yet)"
        )
        return Report(
            (),
            (),
            (
                "cargo test --workspace --no-fail-fast did NOT finish within its "
                "%.0fs deadline and was killed by process group. Last target "
                "announced: %s. %d target(s) had reported a result by then. A "
                "body that did not finish is not a body that passed — something "
                "is hanging, and this gate blocks until it is found. (Known "
                "shape: a test that reads stdin. Every child here already gets "
                "stdin=/dev/null, so a fresh hang is a fresh defect.)"
                % (completed.deadline, last, parse.result_lines),
            ),
            checked,
        )

    if completed.launch_error:
        return Report(
            (),
            (
                "cargo could not be launched (%s), so the workspace tests were "
                "NOT run. Nothing here is a pass." % completed.launch_error,
            ),
            (),
            checked,
        )

    rc = completed.rc
    if rc is None:
        return Report(
            (),
            (
                "cargo test produced no exit status at all, so the run cannot be "
                "judged. 'no status' is not 'status 0'.",
            ),
            (),
            checked,
        )

    if rc < 0:
        return Report(
            (),
            (
                "cargo test was KILLED BY SIGNAL %d (%s) after %d target(s) had "
                "reported a result. The run is incomplete, so its result is "
                "unknown — not clean."
                % (-rc, _signal_name(-rc), parse.result_lines),
            ),
            (),
            checked,
        )

    if rc != 0 and parse.result_lines == 0:
        return Report(
            (),
            (
                "cargo test exited %d without printing a single `test result:` "
                "line — the workspace never got as far as RUNNING tests (build "
                "or link error). No test failure count is reported here, "
                "because none was observed and none may be inferred: this is "
                "UNDETERMINED, not a clean run. Its full output is printed "
                "above." % rc,
            ),
            (),
            checked,
        )

    if rc == 0 and (parse.reported_failed or parse.failed_names):
        return Report(
            (),
            (
                "cargo test exited 0 but its output reports %d failed test(s) "
                "(%d of them named). Exit status and output CONTRADICT each "
                "other, so neither is trusted."
                % (parse.reported_failed, len(parse.failed_names)),
            ),
            (),
            checked,
        )

    if rc != 0 and not parse.reported_failed and not parse.failed_names:
        return Report(
            (),
            (
                "cargo test exited %d but its output reports no failing test at "
                "all. Exit status and output CONTRADICT each other, so neither "
                "is trusted." % rc,
            ),
            (),
            checked,
        )

    if parse.reported_failed != len(parse.failed_names):
        return Report(
            (),
            (
                "cargo test reported %d failing test(s) in its `test result:` "
                "lines but only %d could be NAMED from its output. The list this "
                "gate would print is incomplete, so the result is undetermined "
                "rather than a partial red."
                % (parse.reported_failed, len(parse.failed_names)),
            ),
            (),
            checked,
        )

    if parse.failed_names:
        return Report(
            tuple("%s :: %s" % (target, name) for target, name in parse.failed_names),
            (),
            (),
            checked,
        )

    if parse.result_lines == 0:
        return Report(
            (),
            (
                "cargo test exited 0 but printed no `test result:` line at all — "
                "no target reported running a single test, so there is nothing "
                "here to call green.",
            ),
            (),
            checked,
        )

    return Report((), (), (), checked)


def discover_python_suites(scripts_dir):
    """Find the scripts/test_*.py suites. Returns (modules, problem).

    `modules` is a sorted tuple of dotted module names ("scripts.test_x"), the
    package part taken from the directory's own basename. `problem` is None on
    success and a string otherwise, and when it is a string `modules` is EMPTY
    AND MUST NOT BE RUN AS IF IT WERE THE ANSWER.

    Zero discovered suites is a PROBLEM, never a quiet success. This repo has
    23; a glob that returns nothing means the directory moved, the pattern
    broke, or the checkout is partial — all of which read exactly like "no tests
    to run, therefore fine" if allowed through (CLAUDE.md 3: never return an
    empty collection on error).

    The suites MUST be run as `python3 -m unittest scripts.<module>`, never as
    `python3 scripts/<module>.py`: 13 of the 23 have no
    `if __name__ == "__main__": unittest.main()` block, so the direct form runs
    ZERO tests and exits 0 — a fail-open hiding in the invocation style
    (measured 2026-09-10; filed as backlog 06a9d33f).
    """
    if not scripts_dir:
        return (), (
            "no scripts directory was given, so the python test suites could "
            "not be discovered. An undiscoverable body is not an empty body."
        )
    if not os.path.isdir(scripts_dir):
        return (), (
            "%s is not a readable directory, so the python test suites could "
            "not be discovered. An undiscoverable body is not an empty body."
            % scripts_dir
        )
    try:
        paths = sorted(glob.glob(os.path.join(scripts_dir, "test_*.py")))
    except OSError as exc:
        return (), (
            "could not list %s (%s), so the python test suites could not be "
            "discovered. An undiscoverable body is not an empty body."
            % (scripts_dir, exc)
        )
    if not paths:
        return (), (
            "no test_*.py file was found under %s. This repository ships 23 of "
            "them, so zero means the directory moved, the glob broke, or the "
            "checkout is partial. Reported as a BLOCK, never as 'no suites to "
            "run, therefore fine'." % scripts_dir
        )
    package = os.path.basename(os.path.normpath(scripts_dir))
    return (
        tuple("%s.%s" % (package, os.path.basename(p)[: -len(".py")]) for p in paths),
        None,
    )


_RAN_RE = re.compile(r"^Ran\s+(\d+)\s+tests?\s+in\b")
_FAILNAME_RE = re.compile(r"^(?:FAIL|ERROR):\s+(?P<name>.+?)\s*$")
_IMPORT_FAILURE_MARKER = "unittest.loader._FailedTest"


def parse_unittest_output(text):
    """Extract the summary of one unittest run. Pure. See UnittestParse."""
    ran = None
    outcome = None
    failed_names = []
    text = text or ""

    for raw in text.splitlines():
        line = raw.rstrip()

        m = _RAN_RE.match(line)
        if m:
            ran = int(m.group(1))
            continue
        if line == "OK" or line.startswith("OK ("):
            outcome = "OK"
            continue
        if line == "FAILED" or line.startswith("FAILED ("):
            outcome = "FAILED"
            continue
        m = _FAILNAME_RE.match(line)
        if m:
            failed_names.append(m.group("name"))

    return UnittestParse(
        ran, outcome, tuple(failed_names), _IMPORT_FAILURE_MARKER in text
    )


def python_suite_verdict(module, completed, parse):
    """Fold one suite's Completed plus UnittestParse into a Report.

    Same discipline and same ordering rationale as cargo_verdict: deadline,
    launch failure, no status, signal, failed IMPORT (a suite that never ran, so
    it is neither a pass nor "one failing test"), unparseable, `Ran 0 tests`,
    the two status/output contradictions, FAILED-but-unnameable, FAILED with
    names -> failures, and finally clean.
    """
    if completed.timed_out:
        return Report(
            (),
            (),
            (
                "%s did NOT finish within its %.0fs deadline and was killed by "
                "process group. A suite that hung is not a suite that passed."
                % (module, completed.deadline),
            ),
            1,
        )

    if completed.launch_error:
        return Report(
            (),
            (
                "%s could not be launched (%s), so the suite was NOT run."
                % (module, completed.launch_error),
            ),
            (),
            1,
        )

    rc = completed.rc
    if rc is None:
        return Report(
            (),
            (
                "%s produced no exit status at all, so it cannot be judged. 'no "
                "status' is not 'status 0'." % module,
            ),
            (),
            1,
        )

    if rc < 0:
        return Report(
            (),
            (
                "%s was KILLED BY SIGNAL %d (%s) — the suite did not finish, so "
                "its result is unknown, not clean."
                % (module, -rc, _signal_name(-rc)),
            ),
            (),
            1,
        )

    if parse.import_error:
        return Report(
            (),
            (
                "%s FAILED TO IMPORT (%s), so NONE of its tests ran. This is not "
                "'a test failed' — it is a suite that was never executed, and it "
                "is not counted as one either way. Fix the import and re-run."
                % (module, _IMPORT_FAILURE_MARKER),
            ),
            (),
            1,
        )

    if parse.ran is None or parse.outcome is None:
        return Report(
            (),
            (
                "%s produced output this gate could not parse (no `Ran N tests` "
                "line and/or no OK/FAILED summary), so its result is "
                "undetermined. Its full output is printed above." % module,
            ),
            (),
            1,
        )

    if parse.ran == 0:
        return Report(
            (),
            (
                "%s reported `Ran 0 tests`. A suite that ran no tests proved "
                "nothing, so it is not counted as a pass." % module,
            ),
            (),
            1,
        )

    if parse.outcome == "OK" and rc != 0:
        return Report(
            (),
            (
                "%s printed OK but exited %d. Exit status and output CONTRADICT "
                "each other, so neither is trusted." % (module, rc),
            ),
            (),
            1,
        )

    if parse.outcome == "FAILED" and rc == 0:
        return Report(
            (),
            (
                "%s printed FAILED but exited 0. Exit status and output "
                "CONTRADICT each other, so neither is trusted." % module,
            ),
            (),
            1,
        )

    if parse.outcome == "FAILED":
        if not parse.failed_names:
            return Report(
                (),
                (
                    "%s reported FAILED but this gate could not extract a single "
                    "FAIL:/ERROR: name from its output, so the red cannot be "
                    "named. Undetermined rather than an unnamed red." % module,
                ),
                (),
                1,
            )
        return Report(
            tuple("%s :: %s" % (module, name) for name in parse.failed_names),
            (),
            (),
            1,
        )

    return Report((), (), (), 1)


def _print_verbatim(label, text):
    print("\n----- verbatim output of %s -----" % label, file=sys.stderr)
    print(
        text if (text or "").strip() else "(the child produced no output at all)",
        file=sys.stderr,
    )
    print("----- end verbatim output -----", file=sys.stderr)


def run_cargo_body(repo, runner, cargo):
    """Run the cargo body once and report. `cargo` is a Resolved."""
    if cargo.problem:
        return Report((), (cargo.problem,), (), 0)

    argv = (cargo.path,) + CARGO_ARGV
    env = dict(os.environ)
    bindir = os.path.dirname(cargo.path)
    if bindir:
        env["PATH"] = bindir + os.pathsep + env.get("PATH", "")

    print(
        "check-workspace-tests: running `cargo %s` (deadline %.0fs, "
        "stdin=/dev/null)..." % (" ".join(CARGO_ARGV), CARGO_DEADLINE),
        file=sys.stderr,
    )
    started = time.time()
    completed = runner(argv, repo, CARGO_DEADLINE, env)
    parse = parse_cargo_test_output(completed.out)
    report = cargo_verdict(completed, parse)
    elapsed = time.time() - started

    if report.failures or report.undetermined or report.deadline:
        _print_verbatim("cargo " + " ".join(CARGO_ARGV), completed.out)
    else:
        print(
            "check-workspace-tests: cargo body finished in %.0fs — %d target(s), "
            "0 failing tests." % (elapsed, report.checked),
            file=sys.stderr,
        )
    return report


def run_python_body(repo, runner, python):
    """Run every discovered suite and report. `python` is a Resolved."""
    if python.problem:
        return Report((), (python.problem,), (), 0)

    modules, problem = discover_python_suites(os.path.join(repo, "scripts"))
    if problem:
        # Discovery failing is a BLOCK, not an empty run. Returning the empty
        # `modules` here without the problem would print as "0 suites, all
        # green" — the exact fail-open shape this gate exists to remove.
        return Report((), (problem,), (), 0)

    print(
        "check-workspace-tests: running %d python suite(s) (deadline %.0fs each, "
        "stdin=/dev/null)..." % (len(modules), PYTHON_SUITE_DEADLINE),
        file=sys.stderr,
    )
    started = time.time()
    failures = []
    undetermined = []
    deadline = []
    for module in modules:
        argv = (python.path, "-m", "unittest", module)
        completed = runner(argv, repo, PYTHON_SUITE_DEADLINE, None)
        parse = parse_unittest_output(completed.out)
        report = python_suite_verdict(module, completed, parse)
        if report.failures or report.undetermined or report.deadline:
            _print_verbatim("python3 -m unittest " + module, completed.out)
        failures.extend(report.failures)
        undetermined.extend(report.undetermined)
        deadline.extend(report.deadline)

    if not failures and not undetermined and not deadline:
        print(
            "check-workspace-tests: python body finished in %.0fs — %d suite(s), "
            "0 failures." % (time.time() - started, len(modules)),
            file=sys.stderr,
        )
    return Report(tuple(failures), tuple(undetermined), tuple(deadline), len(modules))


def _print_block(header, items, fix):
    print("\n%s (%d):" % (header, len(items)), file=sys.stderr)
    for item in items:
        print("  - %s" % item, file=sys.stderr)
    print("\nFix: %s" % fix, file=sys.stderr)


def main(argv=None, runner=None, repo=None):
    """Run both bodies, print everything, return the highest-ranked class."""
    argv = [] if argv is None else list(argv)
    runner = run_process if runner is None else runner
    repo = REPO if repo is None else repo

    if argv:
        print(
            "check-workspace-tests: BLOCKED — this script takes no arguments, "
            "and %r was passed. It has no skip switch, no warn-only mode and no "
            "environment variable that weakens its verdict: a gate with an off "
            "switch is not a gate (CLAUDE.md 4). If a commit genuinely must go "
            "out ahead of a red, this repo already has exactly one mechanism "
            "for that (the bypass ledger, scripts/gate-bypass.py) and it is not "
            "this script. The argument is REFUSED rather than ignored, because "
            "an ignored argument is indistinguishable from an honoured one."
            % (argv,),
            file=sys.stderr,
        )
        return RC_ENVIRONMENT

    missing = [
        d for d in ("crates", "scripts") if not os.path.isdir(os.path.join(repo, d))
    ]
    if missing:
        print(
            "check-workspace-tests: BLOCKED — %s does not look like this "
            "repository (missing: %s), so NEITHER test body could even be "
            "located. Nothing below this line was computed, and that is not a "
            "pass." % (repo, ", ".join(missing)),
            file=sys.stderr,
        )
        return RC_ENVIRONMENT

    # Called through the module globals on purpose — see the docstring's
    # testability note. A local alias here would remove the injection point.
    cargo_report = run_cargo_body(repo, runner, resolve_cargo(repo=repo))
    python_report = run_python_body(repo, runner, resolve_python())

    # Everything that fired is printed, whatever the ranking below decides the
    # exit code should be. Suppressing a class because another outranked it
    # would rebuild the invisibility this gate was written to remove.
    if cargo_report.deadline or python_report.deadline:
        _print_block(
            "DEADLINE EXCEEDED — a child was killed after hanging",
            tuple(cargo_report.deadline) + tuple(python_report.deadline),
            "find the hang. This is NOT fixed by re-running and NOT fixed by "
            "lengthening a timeout (there is no switch for that, on purpose). A "
            "test that blocks on stdin is the known shape; every child here is "
            "already given stdin=/dev/null, so a new hang is a new defect.",
        )

    if cargo_report.undetermined:
        _print_block(
            "CARGO BODY UNDETERMINED — the tests did not yield a verdict",
            cargo_report.undetermined,
            "make `cargo test --workspace --no-fail-fast` runnable and readable "
            "again. None of this is '0 failures': a checker that crashed is not "
            "a checker that passed (CLAUDE.md 3).",
        )

    if python_report.undetermined:
        _print_block(
            "PYTHON BODY UNDETERMINED — suites did not yield a verdict",
            python_report.undetermined,
            "repair the suites or the discovery named above. A suite that did "
            "not import, did not finish, or ran zero tests has proved nothing.",
        )

    if cargo_report.failures:
        _print_block(
            "CARGO TEST FAILURES (target :: test)",
            cargo_report.failures,
            "fix these tests, or the code they caught. Reproduce one with "
            "`cargo test -p <crate> <test name> -- --exact < /dev/null` (the "
            "redirect matters — see this script's STDIN note).",
        )

    if python_report.failures:
        _print_block(
            "PYTHON SUITE FAILURES (module :: test)",
            python_report.failures,
            "fix these. Reproduce one with `python3 -m unittest "
            "scripts.<module>` from the repository root.",
        )

    # Green lines are claims about a POPULATION, so each names the size of the
    # population it covers and is printed only when that population was fully
    # inspected.
    if not (
        cargo_report.failures or cargo_report.undetermined or cargo_report.deadline
    ):
        print(
            "OK: cargo body green — %d target(s) ran and every test in them "
            "passed." % cargo_report.checked
        )
    if not (
        python_report.failures or python_report.undetermined or python_report.deadline
    ):
        print(
            "OK: python body green — %d suite(s) ran and every test in them "
            "passed." % python_report.checked
        )

    # Ranking: 5 > 6 > 3 > 4 > 1 > 2. Undetermined outranks failure because a
    # named red printed over a body that never ran is the lie this gate exists
    # to stop. Every class above already printed in full.
    if cargo_report.deadline or python_report.deadline:
        return RC_DEADLINE
    if cargo_report.undetermined:
        return RC_CARGO_UNDETERMINED
    if python_report.undetermined:
        return RC_PYTHON_UNDETERMINED
    if cargo_report.failures:
        return RC_CARGO_FAILURE
    if python_report.failures:
        return RC_PYTHON_FAILURE
    return RC_OK


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
