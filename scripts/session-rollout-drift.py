#!/usr/bin/env python3
"""Surface plugin-rollout drift at SESSION START, where an operator can act.

Why here and not at push (backlog b6a80342). `.githooks/pre-push` demoted
rollout drift (rc=1) from BLOCK to advisory on 2026-07-23, and that demotion was
right: drift is a property of THIS machine's plugin cache, while `git push`
shares code with a remote. Local deploy state must not decide whether a good
commit can be shared (CLAUDE.md 7 — the flow-control authority stays local, and
it also must not sit on an operation that is about something else).

But demoting alone regressed the ORIGINAL failure mode. Drift is exactly the
class of fault nothing goes red for: 16 plugins once drifted silently across
~20 commits, and the only reason anyone found out was that someone looked. A
demoted check with no new home is "someone might notice", which CLAUDE.md 6
names as the thing not to rely on. Detection itself was never the problem —
`scripts/check-plugin-rollout.py` survives intact — so this moves the REPORT to
the moment an operator is present and can run `scripts/rollout-plugins.sh`.

Silence is load-bearing here, in both directions:

  * No drift  -> print NOTHING. A notice that fires every session is not a
    detection; it is wallpaper, and it trains the reader to skip the one time it
    matters.
  * Drift     -> print it, name the plugins, name the remedy.
  * Could not tell -> print THAT, explicitly. This is the case the design hinges
    on: staying silent on an unverifiable check makes "I could not look" and "I
    looked and it was clean" the same bytes, which is the exact fail-open shape
    CLAUDE.md 3 forbids (and the one `3b1eb24` already hit once, when an empty
    statusline read as "plenty of budget").

Exit status is always 0. This is a NOTIFICATION surface, not a gate: it holds no
block/allow authority, so it has nothing to fail closed *to*, and a SessionStart
hook that exits non-zero degrades the session for a report. The verdict it does
carry lives in the TEXT, which is why the undetermined case is printed rather
than swallowed.

Design for testability (scripts/test_session_rollout_drift.py): every process
launch goes through the injected `runner` argument, and `classify` is a pure
function from (rc, output) to a notice. The suite therefore covers every exit
class deterministically and never runs the real checker.
"""

from __future__ import annotations

import os
import subprocess
import sys

CHECKER = "scripts/check-plugin-rollout.py"

# Mirrors scripts/check-plugin-rollout.py's own taxonomy (its RC_* constants).
# Duplicated deliberately rather than imported: that script is not an importable
# module (hyphens in the name), and a wrong mapping here must be visible as a
# wrong mapping, not silently inherited.
RC_OK = 0
RC_ROLLOUT = 1
RC_ENABLEMENT = 2
RC_UNVERIFIABLE = 3
RC_PARKED_CONFIG = 4
RC_RETIRED = 5
RC_RETIRED_CONFIG = 6

# Classes that mean "there is a real, actionable drift". Each has a remedy the
# operator can run right now, which is the whole reason this report moved here.
ACTIONABLE = {
    RC_ROLLOUT: "plugin rollout drift",
    RC_ENABLEMENT: "a GATE crate is not enabled",
    RC_RETIRED: "a retired plugin is still deployed",
}

# Classes where the checker ran but could NOT decide. Reported as undetermined,
# never folded into the clean path.
UNDETERMINED = {
    RC_UNVERIFIABLE: "the checker could not verify the deployed state",
    RC_PARKED_CONFIG: "scripts/parked-plugins.json could not be read",
    RC_RETIRED_CONFIG: "the retired-plugins config could not be read",
}

MAX_BODY_LINES = 24


def _body(output: str) -> str:
    """The checker's own words, truncated but never summarised away."""
    lines = [ln.rstrip() for ln in (output or "").splitlines() if ln.strip()]
    if not lines:
        return "    (the checker produced no output)"
    shown = lines[:MAX_BODY_LINES]
    out = "\n".join("    " + ln for ln in shown)
    if len(lines) > MAX_BODY_LINES:
        out += f"\n    … {len(lines) - MAX_BODY_LINES} more line(s); run {CHECKER} to see all"
    return out


def classify(rc: int | None, output: str) -> str | None:
    """The notice for this outcome, or None when there is nothing to say.

    `rc is None` means the checker could not be launched at all — which is an
    undetermined result, not a clean one.
    """
    if rc == RC_OK:
        return None  # clean, and a clean check says nothing

    if rc is None:
        return (
            "⚠ plugin rollout: UNDETERMINED — could not run "
            f"{CHECKER} at all.\n"
            "  This is not the same as 'no drift'. The deployed plugins may be "
            "stale and nothing here checked.\n" + _body(output)
        )

    if rc in ACTIONABLE:
        return (
            f"⚠ plugin rollout: {ACTIONABLE[rc]} (rc={rc}).\n"
            "  The deployed harness is NOT running your committed code. Fix with:\n"
            "      scripts/rollout-plugins.sh\n" + _body(output)
        )

    if rc in UNDETERMINED:
        return (
            f"⚠ plugin rollout: UNDETERMINED — {UNDETERMINED[rc]} (rc={rc}).\n"
            "  Reported rather than skipped: an unverifiable check is not a "
            "passing one.\n" + _body(output)
        )

    # An exit code this script does not recognise. Resolving it to "clean" would
    # mean a NEW failure class added to the checker arrives here as silence.
    return (
        f"⚠ plugin rollout: UNDETERMINED — {CHECKER} exited {rc}, which this "
        "notice does not recognise.\n"
        "  Treated as undetermined rather than clean; check whether the "
        "checker's exit codes changed.\n" + _body(output)
    )


def _run(argv, cwd, timeout):
    """Default runner. Returns (rc, combined output); rc None = never launched."""
    try:
        p = subprocess.run(
            argv, cwd=cwd, capture_output=True, text=True, timeout=timeout
        )
    except subprocess.TimeoutExpired:
        return None, f"timed out after {timeout}s"
    except (OSError, subprocess.SubprocessError) as e:
        return None, str(e)
    return p.returncode, (p.stdout or "") + (p.stderr or "")


def notice_for_repo(root: str, runner=_run, timeout: float = 60.0) -> str | None:
    """The notice for the repo at `root`, or None if there is nothing to say.

    Returns None when this repo simply has no rollout checker — that is a real
    absence (most repos are not this one), not an unverifiable state.
    """
    checker = os.path.join(root, CHECKER)
    if not os.path.isfile(checker):
        return None
    rc, output = runner(
        (sys.executable or "python3", checker), root, timeout
    )
    return classify(rc, output)


def main() -> int:
    root = sys.argv[1] if len(sys.argv) > 1 else os.getcwd()
    notice = notice_for_repo(root)
    if notice:
        print(notice)
    return 0


if __name__ == "__main__":
    sys.exit(main())
