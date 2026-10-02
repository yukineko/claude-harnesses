#!/usr/bin/env python3
"""Repro test for backlog 4159ee85: installed_plugins.json.bak-* grows without bound.

scripts/rollout-plugins.sh registry_patch() copies the registry to
`<registry>.bak-<epoch>` on every non-dry run and nothing ever prunes them (the
item observed 200+; the auditor 300 on this machine).

The test runs the REAL registry_patch Python body (extracted verbatim from the
heredoc in scripts/rollout-plugins.sh — the rollout script itself is not run)
against a throwaway registry that already has 40 old backups beside it, performs
one real patch, and requires that the backup count did not simply grow (some
retention happened). It does not pin a particular retention number. Open;
expectedFailure.
"""

import json
import re
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROLLOUT = Path(__file__).resolve().parent.parent / "scripts" / "rollout-plugins.sh"


def registry_patch_source():
    text = ROLLOUT.read_text()
    m = re.search(r"^registry_patch\(\) \{\n  python3 - \"\$@\" <<'PY'\n(.*?)\nPY\n\}", text, re.S | re.M)
    if not m:
        raise AssertionError("could not locate registry_patch() heredoc in rollout-plugins.sh")
    return m.group(1)


class BackupRetention(unittest.TestCase):
    @unittest.expectedFailure
    def test_backups_do_not_grow_without_bound(self):
        src = registry_patch_source()
        with tempfile.TemporaryDirectory(prefix="bl-4159ee85-") as d:
            d = Path(d)
            reg = d / "installed_plugins.json"
            reg.write_text(json.dumps({"version": 2, "plugins": {}}))
            for i in range(40):
                (d / ("installed_plugins.json.bak-%d" % (1700000000 + i))).write_text("{}")
            script = d / "registry_patch.py"
            script.write_text(src)
            p = subprocess.run(
                [sys.executable, str(script), str(reg), "owner", "deadbeef",
                 "demo", "1.0.0", str(d / "cache" / "demo" / "1.0.0")],
                capture_output=True, text=True, timeout=60,
            )
            self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
            self.assertIn("registry backup:", p.stdout, "precondition: a backup was written")
            self.assertIn("demo@owner", json.loads(reg.read_text())["plugins"],
                          "precondition: the patch really applied")
            n = len(list(d.glob("installed_plugins.json.bak-*")))
            self.assertLess(
                n, 41,
                "40 old backups + 1 new = %d: registry_patch keeps every backup forever" % n,
            )


if __name__ == "__main__":
    unittest.main()
