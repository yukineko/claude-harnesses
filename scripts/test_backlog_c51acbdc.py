#!/usr/bin/env python3
"""backlog c51acbdc (resolver half): rollout's resolve_overwatch_bin prefers a
DARK `overwatch` launcher on PATH over a runnable binary.

After OVERWATCH_BIN, `resolve_overwatch_bin` returns `command -v overwatch`
without checking that it runs. A dark launcher (no bundled binary for the host:
exits 1 / prints a non-JSON diagnostic) therefore wins over the freshly built
`$REPO/target/release/overwatch`, and run_canary then feeds that launcher's
non-JSON output to `json.load` (JSONDecodeError under set -e) instead of
refusing explicitly.

Only the function is extracted from scripts/rollout-plugins.sh and run in a
temp REPO with stub binaries; the rollout script itself is never executed.
The run_canary JSONDecodeError half is not exercised here.

Run:  python3 -m unittest scripts.test_backlog_c51acbdc
"""
import os
import re
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
ROLLOUT = SCRIPTS / "rollout-plugins.sh"


def extract_function(src, name):
    m = re.search(r"^%s\(\) \{\n.*?^\}\n" % re.escape(name), src, re.S | re.M)
    if not m:
        raise AssertionError(f"function {name} not found in rollout-plugins.sh")
    return m.group(0)


def resolve(tmp, with_dark_on_path):
    fn = extract_function(ROLLOUT.read_text(), "resolve_overwatch_bin")
    repo = tmp / "repo"
    good = repo / "target" / "release" / "overwatch"
    good.parent.mkdir(parents=True)
    good.write_text('#!/bin/sh\necho \'{"stages":[]}\'\n')
    good.chmod(0o755)
    pathdir = tmp / "pathbin"
    pathdir.mkdir()
    if with_dark_on_path:
        dark = pathdir / "overwatch"
        dark.write_text(
            '#!/bin/sh\necho "overwatch: no bundled binary for host" >&2\nexit 1\n')
        dark.chmod(0o755)
    harness = tmp / "h.sh"
    harness.write_text(f'set -euo pipefail\nREPO="{repo}"\n{fn}\nresolve_overwatch_bin\n')
    env = dict(os.environ)
    env.pop("OVERWATCH_BIN", None)
    # Only system dirs + the stub dir: no real overwatch, no cargo.
    env["PATH"] = f"{pathdir}:/usr/bin:/bin"
    r = subprocess.run(["bash", str(harness)], capture_output=True, text=True, env=env)
    return r, str(good), str(pathdir / "overwatch")


class ResolveOverwatchBin(unittest.TestCase):
    def test_control_without_path_launcher_uses_built_binary(self):
        with tempfile.TemporaryDirectory() as t:
            r, good, _ = resolve(Path(t), with_dark_on_path=False)
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(r.stdout.strip(), good)

    @unittest.expectedFailure
    def test_dark_path_launcher_is_not_preferred(self):
        """backlog c51acbdc: open defect (RED observed)."""
        with tempfile.TemporaryDirectory() as t:
            r, good, dark = resolve(Path(t), with_dark_on_path=True)
            self.assertNotEqual(
                r.stdout.strip(), dark,
                f"resolve_overwatch_bin chose the dark PATH launcher over {good}")


if __name__ == "__main__":
    unittest.main()
