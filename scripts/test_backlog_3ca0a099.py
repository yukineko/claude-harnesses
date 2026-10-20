#!/usr/bin/env python3
"""backlog 3ca0a099: rollout's execute_stage_rollback cannot roll back without
a runnable overwatch.

`execute_stage_rollback` computes its restore targets by shelling out to
`"$ow" canary-rollback-plan ...` inside a `$(...)` assignment. Under the
script's `set -euo pipefail`, a dead/dark overwatch makes that assignment fail
and aborts the rollback BEFORE any registry entry is restored, although the
prior version/path it needs is already in the plan rows (cur_version/cur_path).
The stage is then left live, pointing at the canary version.

The function (and only the helpers it calls) is extracted from
scripts/rollout-plugins.sh and run against a temp registry with a stub
overwatch; the rollout script itself is never executed and nothing real is
touched.

Run the open-defect test with:  python3 -m unittest scripts.test_backlog_3ca0a099
"""
import json
import os
import re
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
ROLLOUT = SCRIPTS / "rollout-plugins.sh"
FUNCS = [
    "execute_stage_rollback",
    "row_for_name",
    "build_state_json",
    "rollback_target_lookup",
    "registry_patch",
]


def extract_function(src, name):
    m = re.search(r"^%s\(\) \{\n.*?^\}\n" % re.escape(name), src, re.S | re.M)
    if not m:
        raise AssertionError(f"function {name} not found in rollout-plugins.sh")
    return m.group(0)


def run_rollback(tmp, ow_body):
    src = ROLLOUT.read_text()
    funcs = "\n".join(extract_function(src, f) for f in FUNCS)
    cache = tmp / "cache"
    prior = cache / "foo" / "0.1.0"
    canary = cache / "foo" / "0.2.0"
    prior.mkdir(parents=True)
    canary.mkdir(parents=True)
    reg = tmp / "installed_plugins.json"
    # The stage was applied: the registry points at the canary version.
    reg.write_text(json.dumps({"version": 1, "plugins": {"foo@yukineko": [
        {"scope": "user", "installPath": str(canary), "version": "0.2.0"}]}}))
    ow = tmp / "ow"
    ow.write_text("#!/usr/bin/env bash\n" + ow_body)
    ow.chmod(0o755)
    row = "\t".join(["foo", "0.2.0", "crates/foo", str(canary), "1", "1", "0",
                     "", "", "0.1.0", str(prior)])
    harness = tmp / "harness.sh"
    harness.write_text(
        "set -euo pipefail\n" + funcs + "\n"
        f'REGISTRY="{reg}"\nOWNER=yukineko\nGIT_SHA=deadbeef\n'
        f"PLAN_ROWS=($'{row}')\n"
        f'execute_stage_rollback "{ow}" 0 "foo" 0\n'
    )
    r = subprocess.run(["bash", str(harness)], capture_output=True, text=True)
    entry = json.loads(reg.read_text())["plugins"]["foo@yukineko"][0]
    return r, entry, str(prior)


GOOD_PLAN = r"""cat <<JSON
{"targets":[{"name":"foo","is_new":false,"prior_version":"0.1.0","restore_install_path":"$PRIOR"}]}
JSON
"""


class ExecuteStageRollback(unittest.TestCase):
    def test_control_rollback_restores_prior_with_working_overwatch(self):
        with tempfile.TemporaryDirectory() as t:
            t = Path(t)
            prior = t / "cache" / "foo" / "0.1.0"
            r, entry, _ = run_rollback(t, GOOD_PLAN.replace("$PRIOR", str(prior)))
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(entry["installPath"], str(prior), (entry, r.stdout, r.stderr))

    @unittest.expectedFailure
    def test_rollback_restores_prior_even_when_overwatch_is_dead(self):
        """backlog 3ca0a099: open defect (RED observed)."""
        with tempfile.TemporaryDirectory() as t:
            t = Path(t)
            r, entry, prior = run_rollback(
                t, 'echo "overwatch: no bundled binary for darwin-arm64" >&2\nexit 1\n')
            self.assertEqual(
                entry["installPath"], prior,
                f"stage left live on the canary version: rc={r.returncode} "
                f"stderr={r.stderr!r} entry={entry}")


if __name__ == "__main__":
    unittest.main()
