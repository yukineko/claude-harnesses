"""Repros for two blind spots of scripts/tests/launcher-missing-binary-failclosed.sh.

The script derives REPO from its own location, so each test copies the CURRENT script
into a throwaway repo whose crates/*/bin hold deliberately broken launchers, runs it,
and checks whether it notices. A regression test that stays GREEN on a broken subject
proves nothing about that subject.

2b43930c: for a non-SessionEnd hook the script passes any run that is not
  "rc=0 + empty stdout": only JSON shape and hookEventName are checked. A launcher that
  answers a hook with exit 0 and unrelated JSON (nothing saying the binary did NOT run)
  is reported GREEN.
f81a8197: there is no control case with the per-platform binary PRESENT, so a launcher
  that never execs its binary (always says "did NOT run" and exits non-zero) is GREEN.

Control: a launcher that is silent (rc=0, empty stdout) on the hook path is RED, which
proves the fake repo is enumerated and the script's existing assertion fires.
"""
import json
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
SUBJECT = HERE / "tests" / "launcher-missing-binary-failclosed.sh"
N = 32  # the script requires >20 hooks and >30 launchers


def fake_repo(launcher_body):
    root = Path(tempfile.mkdtemp(prefix="launcher-ntE-")).resolve()
    (root / "scripts" / "tests").mkdir(parents=True)
    shutil.copy2(SUBJECT, root / "scripts" / "tests" / SUBJECT.name)
    for i in range(N):
        name = f"fake{i}"
        c = root / "crates" / name
        (c / "bin").mkdir(parents=True)
        (c / "hooks").mkdir()
        lp = c / "bin" / name
        lp.write_text(launcher_body.replace("@NAME@", name))
        lp.chmod(0o755)
        (c / "hooks" / "hooks.json").write_text(json.dumps({"hooks": {"PreToolUse": [
            {"hooks": [{"type": "command", "command": "${CLAUDE_PLUGIN_ROOT}/bin/" + name + " hook"}]}]}}))
    return root


def run_subject(launcher_body):
    root = fake_repo(launcher_body)
    try:
        r = subprocess.run(["bash", str(root / "scripts" / "tests" / SUBJECT.name)],
                           capture_output=True, text=True, timeout=300)
    finally:
        shutil.rmtree(root, ignore_errors=True)
    return r


SILENT = """#!/bin/sh
case "$1" in hook) exit 0 ;; esac
echo "@NAME@: binary missing" >&2; exit 3
"""

UNRELATED_JSON = """#!/bin/sh
case "$1" in hook) echo '{"unrelated":true}'; exit 0 ;; esac
echo "@NAME@: binary missing" >&2; exit 3
"""

# Never looks for, let alone execs, <name>-<os>-<arch>: always announces "did NOT run".
NEVER_EXECS = """#!/bin/sh
echo '{"systemMessage":"@NAME@: binary missing - did NOT run (UNKNOWN)","hookSpecificOutput":{"hookEventName":"PreToolUse","additionalContext":"@NAME@ did NOT run (UNKNOWN)"}}'
echo "@NAME@: binary missing - did NOT run" >&2
exit 1
"""


class LauncherFailclosedBlindSpots(unittest.TestCase):
    def test_control_silent_launcher_is_red(self):
        r = run_subject(SILENT)
        self.assertNotEqual(r.returncode, 0, r.stdout[-600:])
        self.assertIn("exit 0 with EMPTY stdout", r.stdout)

    @unittest.expectedFailure  # backlog 2b43930c: open defect, remove when fixed
    def test_exit0_with_unrelated_json_is_red(self):
        r = run_subject(UNRELATED_JSON)
        self.assertNotEqual(r.returncode, 0, "GREEN on a launcher that answers hooks with unrelated JSON:\n"
                            + r.stdout[-400:])

    @unittest.expectedFailure  # backlog f81a8197: open defect, remove when fixed
    def test_launcher_that_never_execs_its_binary_is_red(self):
        r = run_subject(NEVER_EXECS)
        self.assertNotEqual(r.returncode, 0, "GREEN on a launcher that never execs its binary:\n"
                            + r.stdout[-400:])


if __name__ == "__main__":
    unittest.main()
