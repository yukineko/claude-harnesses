#!/usr/bin/env python3
"""backlog d38c195b: .githooks/pre-push's `1)` branch for check-plugin-rollout.py
always appends "reaches no session here until you roll it out:
scripts/rollout-plugins.sh --plugin <name>". Since 5461ba1c the checker also
exits 1 (RC_ROLLOUT) for an orphan-only finding -- a cached plugin with NO crate
under crates/ -- whose own Fix line says "this is NOT fixed by a rollout". The
hook then contradicts the checker and pushes the reader to re-arm a removed
plugin.

Drives the real hook (copied into a throwaway repo by the existing
test_prepush_hook.HookHarness) with a check-plugin-rollout.py stub that exits 1
and prints ONLY an ORPHAN CACHE DIR block (the checker's real wording, no
ROLLOUT DRIFT section). Only stderr is asserted: the push verdict is not the
property under test.

Repro:  python3 -m pytest --runxfail scripts/test_backlog_d38c195b.py
"""

from __future__ import annotations

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import test_prepush_hook as tph  # noqa: E402

_ORPHAN_ONLY_STUB = '''#!/usr/bin/env python3
import sys
sys.stderr.write(
    "\\nORPHAN CACHE DIR (1 problem(s)): the plugin cache holds dirs for a plugin "
    "that has no crate under crates/:\\n"
    "  - gonezo: cached but no current version known from crates/\\n"
    "\\nFix: this is NOT fixed by a rollout - with no crate there is nothing to "
    "roll out. Either delete the orphan cache dir, or declare the retirement in "
    "scripts/retired-plugins.json.\\n"
)
sys.exit(1)
'''


class OrphanOnlyRc1(tph.HookTestCase):
    def _run_orphan_only(self):
        h = self.harness(gate_bypass_exit=0, rollout_missing=False, rollout_exit=1)
        stub = h.root / "scripts" / "check-plugin-rollout.py"
        stub.write_text(_ORPHAN_ONLY_STUB)
        stub.chmod(0o755)
        return h.run()

    def test_control_checker_output_reaches_stderr(self):
        # Anti-vacuity: the hook really ran the stub and echoed it, so the
        # absence asserted below is not "the block never ran".
        proc = self._run_orphan_only()
        self.assertIn("ORPHAN CACHE DIR", proc.stderr, proc.stderr)

    @unittest.expectedFailure
    def test_orphan_only_finding_is_not_told_to_roll_out(self):
        proc = self._run_orphan_only()
        self.assertNotIn(
            "until you roll it out",
            proc.stderr,
            "an orphan-only rc=1 has no crate to roll out; the hook's rollout "
            "advisory contradicts the checker's own Fix line\n" + proc.stderr,
        )


if __name__ == "__main__":
    unittest.main(verbosity=1)
