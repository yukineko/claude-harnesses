#!/usr/bin/env python3
"""Unit tests for scripts/validate-manifests.py.

Stdlib-only (`unittest`), no third-party dependency, so it runs identically in
CI and locally:  `python3 scripts/test_validate_manifests.py`.

The module under test has a hyphen in its name, so it is loaded via importlib
rather than a plain `import`.
"""

from __future__ import annotations

import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_SPEC = importlib.util.spec_from_file_location(
    "validate_manifests", _HERE / "validate-manifests.py"
)
vm = importlib.util.module_from_spec(_SPEC)
# Compiled from the SOURCE TEXT, deliberately not via the spec loader's
# exec_module: SourceFileLoader reuses a __pycache__ .pyc validated only by
# (source mtime at 1 s granularity, size), so a same-second size-preserving
# edit (e.g. a reordering mutant) would run stale bytecode -- a false GREEN /
# false mutation SURVIVOR, never a false red. Backlog 05726f9f; do not
# "simplify" this back. get_source() reads the .py, never the cache;
# dont_inherit keeps this file's __future__ flags off the subject.
exec(  # noqa: S102
    compile(_SPEC.loader.get_source(_SPEC.name), _SPEC.origin, "exec", dont_inherit=True),
    vm.__dict__,
)


def _write_manifest(tmp: Path, payload) -> Path:
    """Write a plugin.json fixture under a path inside REPO (for relative_to)."""
    path = tmp / "plugin.json"
    path.write_text(json.dumps(payload), encoding="utf-8")
    return path


class CheckPluginManifestTests(unittest.TestCase):
    def setUp(self):
        # Fixtures must live under REPO: check_plugin_manifest calls
        # path.relative_to(REPO) to build error messages.
        self._tmp = tempfile.TemporaryDirectory(dir=vm.REPO)
        self.tmp = Path(self._tmp.name)

    def tearDown(self):
        self._tmp.cleanup()

    def test_valid_manifest_has_no_errors(self):
        errors: list[str] = []
        path = _write_manifest(
            self.tmp, {"name": "run-book", "description": "a plugin", "version": "1.2.3"}
        )
        name = vm.check_plugin_manifest(path, errors)
        self.assertEqual(name, "run-book")
        self.assertEqual(errors, [])

    def test_version_is_optional(self):
        errors: list[str] = []
        _write_manifest(self.tmp, {"name": "ok", "description": "d"})
        vm.check_plugin_manifest(self.tmp / "plugin.json", errors)
        self.assertEqual(errors, [])

    def test_missing_name_reported(self):
        errors: list[str] = []
        path = _write_manifest(self.tmp, {"description": "d"})
        self.assertIsNone(vm.check_plugin_manifest(path, errors))
        self.assertTrue(any("missing required field 'name'" in e for e in errors))

    def test_non_kebab_name_reported(self):
        errors: list[str] = []
        path = _write_manifest(self.tmp, {"name": "Bad_Name", "description": "d"})
        vm.check_plugin_manifest(path, errors)
        self.assertTrue(any("not kebab-case" in e for e in errors))

    def test_missing_description_reported(self):
        errors: list[str] = []
        path = _write_manifest(self.tmp, {"name": "ok"})
        vm.check_plugin_manifest(path, errors)
        self.assertTrue(any("missing required field 'description'" in e for e in errors))

    def test_bad_semver_reported(self):
        errors: list[str] = []
        path = _write_manifest(self.tmp, {"name": "ok", "description": "d", "version": "v1"})
        vm.check_plugin_manifest(path, errors)
        self.assertTrue(any("is not semver" in e for e in errors))

    def test_invalid_json_reported(self):
        errors: list[str] = []
        path = self.tmp / "plugin.json"
        path.write_text("{not json", encoding="utf-8")
        self.assertIsNone(vm.check_plugin_manifest(path, errors))
        self.assertTrue(any("invalid JSON" in e for e in errors))


class KebabAndSemverTests(unittest.TestCase):
    def test_kebab_regex(self):
        self.assertTrue(vm.KEBAB.match("run-book"))
        self.assertTrue(vm.KEBAB.match("abc123"))
        self.assertFalse(vm.KEBAB.match("Run-Book"))
        self.assertFalse(vm.KEBAB.match("-leading"))
        self.assertFalse(vm.KEBAB.match("double--dash"))

    def test_semver_regex(self):
        self.assertTrue(vm.SEMVER.match("1.2.3"))
        self.assertTrue(vm.SEMVER.match("0.1.0-rc.1"))
        self.assertFalse(vm.SEMVER.match("1.2"))
        self.assertFalse(vm.SEMVER.match("v1.2.3"))


if __name__ == "__main__":
    unittest.main()
