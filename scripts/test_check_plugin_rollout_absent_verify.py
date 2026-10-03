#!/usr/bin/env python3
"""Independent verifier test for backlog 73c2c089 (written by the verifier, not
the implementer).

Claim under test: when the plugin registry or settings.json that
scripts/check-plugin-rollout.py is pointed at (via CLAUDE_PLUGIN_REGISTRY /
CLAUDE_SETTINGS) does not exist, the checker must NOT exit 0. An absent input
means the dimension could not be checked; "could not check" is not "clean".

How the paths reach the checker: through the ENVIRONMENT, exactly as an
operator (or an attacker) would select them. The checker reads those env vars
at import time, so every case loads a FRESH module instance after setting them,
and asserts the module really picked them up (otherwise the absent cases would
silently test the developer's real ~/.claude files).

Everything else that would otherwise reach the real machine (crates/, plugin
cache, git provenance query, shared-crate version) is pointed at a synthetic
fixture built by the pre-existing `_make_fixture` helper of
scripts/test_check_plugin_rollout.py.

Anti-vacuity: the same fixture with BOTH files present must exit 0. If it did
not, the absent cases' non-zero rc could come from some unrelated finding in the
fixture and would prove nothing about absence.
"""
import importlib.util
import io
import os
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

_HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(_HERE))
import test_check_plugin_rollout as fixture_mod  # noqa: E402

CHECKER = _HERE / "check-plugin-rollout.py"
ENV_KEYS = ("CLAUDE_PLUGIN_REGISTRY", "CLAUDE_SETTINGS", "PARKED_PLUGINS", "RETIRED_PLUGINS")


def _fresh_checker():
    spec = importlib.util.spec_from_file_location("cpr_absent_verify", CHECKER)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


class AbsentInputIsNotExitZero(unittest.TestCase):
    def setUp(self):
        self._saved_env = {k: os.environ.get(k) for k in ENV_KEYS}

    def tearDown(self):
        for k, v in self._saved_env.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v

    def _run(self, *, registry_present, settings_present):
        with tempfile.TemporaryDirectory(prefix="verify-73c2c089-") as d:
            tmp = Path(d)
            crates, reg, sett = fixture_mod._make_fixture(
                tmp, write_registry=registry_present, write_settings=settings_present
            )
            if not registry_present:
                reg = tmp / "no-such-dir" / "installed_plugins.json"
            if not settings_present:
                sett = tmp / "no-such-dir" / "settings.json"
            self.assertEqual(reg.exists(), registry_present)
            self.assertEqual(sett.exists(), settings_present)
            os.environ["CLAUDE_PLUGIN_REGISTRY"] = str(reg)
            os.environ["CLAUDE_SETTINGS"] = str(sett)
            # Absent declaration files = "nothing parked / nothing retired", so
            # the repo's real declarations cannot reclassify fixture findings.
            os.environ["PARKED_PLUGINS"] = str(tmp / "parked-plugins.json")
            os.environ["RETIRED_PLUGINS"] = str(tmp / "retired-plugins.json")

            cpr = _fresh_checker()
            # The env vars must be what the checker actually reads.
            self.assertEqual(cpr.REGISTRY_PATH, str(reg))
            self.assertEqual(cpr.SETTINGS_PATH, str(sett))

            cpr.CRATES = str(crates)
            cpr.PLUGIN_CACHE_ROOT = str(tmp / "cache")
            cpr.SOURCE_CHANGED_SINCE = lambda commit, crate: False
            cpr.SOURCE_CORE_VERSION = lambda: "0.2.1"
            cpr.CORE_VERSION_IN_HISTORY = lambda version: None
            out, err = io.StringIO(), io.StringIO()
            with redirect_stdout(out), redirect_stderr(err):
                rc = cpr.main()
            return rc, out.getvalue() + err.getvalue()

    def test_control_both_present_is_clean(self):
        rc, output = self._run(registry_present=True, settings_present=True)
        self.assertEqual(rc, 0, "control fixture must be clean, else absence tests are vacuous:\n" + output)

    def test_absent_registry_only_is_not_exit_zero(self):
        rc, output = self._run(registry_present=False, settings_present=True)
        self.assertNotEqual(rc, 0, output)

    def test_absent_settings_only_is_not_exit_zero(self):
        rc, output = self._run(registry_present=True, settings_present=False)
        self.assertNotEqual(rc, 0, output)

    def test_both_absent_is_not_exit_zero(self):
        rc, output = self._run(registry_present=False, settings_present=False)
        self.assertNotEqual(rc, 0, output)


if __name__ == "__main__":
    unittest.main()
