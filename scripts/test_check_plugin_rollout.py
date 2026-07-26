#!/usr/bin/env python3
"""Unit tests for scripts/check-plugin-rollout.py.

Stdlib-only (`unittest`), no network. Exercises both dimensions the script
checks, against synthetic fixture repos/registries/settings (never the real
~/.claude state):

  1. ROLLOUT — source plugin.json version vs the deployed registry version.
     Drift and never-installed are hard failures; an absent registry is a
     fail-soft skip.
  2. ENABLEMENT — the demonstrated hole. A GATE crate missing from (or set
     false in) `enabledPlugins` is a hard failure; a non-gate plugin is a
     warning only; an absent settings.json is a fail-soft skip.

The script reads its paths from module-level constants resolved at import
time, so each test rebinds them (rather than setting env vars) and restores
them afterwards.
"""
import importlib.util
import io
import json
import os
import platform
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_SPEC = importlib.util.spec_from_file_location(
    "check_plugin_rollout", _HERE / "check-plugin-rollout.py"
)
cpr = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(cpr)

OWNER = "yukineko"

# The fixture fleet must contain EVERY expected GATE plugin: the script now
# reconciles the plugins it found against EXPECTED_GATE_PLUGINS and fails on any
# gate it could not account for, so a fixture missing one is (correctly) drift
# rather than a clean baseline. `mutategate` is deliberately absent — it ships no
# plugin.json and is the exempted non-plugin gate.
# The `<os>-<arch>` suffix rebuild-plugins.sh gives this host's binary. Computed
# here rather than imported from the script under test: a fixture that borrowed
# the checker's own constant would agree with it by construction, including when
# both are wrong, and would also make the suite un-runnable against a build of
# the checker that predates the constant.
def _host_suffix_for_fixture():
    sysname = platform.system().lower()
    os_part = sysname if sysname in ("darwin", "linux", "windows") else "unknown"
    mach = platform.machine().lower()
    if mach in ("x86_64", "amd64"):
        return f"{os_part}-x86_64"
    if mach in ("aarch64", "arm64"):
        return f"{os_part}-arm64"
    return f"{os_part}-unknown"


_HOST_SUFFIX = _host_suffix_for_fixture()

FIXTURE_PLUGINS = {
    "blastguard": "1.2.0",   # GATE
    "propguard": "0.9.1",    # GATE
    "specguard": "2.1.0",    # GATE
    "stuckguard": "0.4.2",   # GATE
    "overwatch": "5.0.1",    # GATE
    "condukt": "3.0.0",      # non-gate
    "benchkit": "0.1.0",     # non-gate (the kind users disable on purpose)
}


def _write_json(path, data):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, indent=2), encoding="utf-8")


# A clean provenance manifest: the deployed binary was built from a committed
# tree, at a commit the stub `source_changed` below reports as unchanged.
_DEFAULT_PROVENANCE = {"commit": "cafef00d" * 5, "dirty": False, "deployed_at": 0}


def _changed_stub(changed_crates=()):
    """Stand in for the git query, so provenance is testable without a repo.

    Returns a callable matching SOURCE_CHANGED_SINCE's contract:
    (commit, crate) -> True (source moved since), False (unchanged), or None
    (could not determine).
    """
    changed = dict(changed_crates) if isinstance(changed_crates, dict) else {
        c: True for c in changed_crates
    }

    def _fn(commit, crate):
        return changed.get(crate, False)

    return _fn


def _make_fixture(tmp, *, versions=None, registry_versions=None, enabled=None,
                  enabled_absent=(), enabled_plugins_raw=None,
                  write_registry=True, write_settings=True,
                  registry_text=None, settings_text=None,
                  corrupt_plugin_json=(), drop_plugin_json=(),
                  omit_version=(), registry_raw_entries=None,
                  provenance=None, provenance_text=None,
                  install_paths=True,
                  skill_only=(), no_host_binary=(), force_host_binary=()):
    """Build a fixture repo + registry + settings under `tmp`.

    versions           — crate -> source version (defaults to FIXTURE_PLUGINS)
    registry_versions  — crate -> deployed version; None entry = not installed.
                         Defaults to matching the source exactly (no drift).
    enabled            — crate -> bool OVERRIDES merged over "everything True".
    enabled_absent     — crates to omit from enabledPlugins entirely (the
                         real-world propguard case: absent, not false).
    enabled_plugins_raw— write this exact value as `enabledPlugins` (used to
                         exercise a non-dict shape).
    registry_text /
    settings_text      — write this raw text instead of JSON (malformed cases).
    corrupt_plugin_json— crates whose plugin.json is written as invalid JSON.
    drop_plugin_json   — crates whose plugin.json is not written at all.
    omit_version       — crates whose plugin.json carries a name but no
                         "version" key at all.
    registry_raw_entries— crate -> the exact value to store as the registry
                         entry (used for malformed entry shapes).
    skill_only         — crates that ship NO Rust binary (no src/main.rs, no
                         Cargo.toml), like the real daily-report and scout.
                         Every other crate gets both a binary target in the
                         source AND a deployed host binary, so the provenance
                         dimension actually applies to it — without that, the
                         whole class would be classified skill-only and every
                         provenance assertion below would pass vacuously.
    no_host_binary     — crates that DECLARE a binary in the source but have
                         none deployed (the "installed but execs nothing" case).
    """
    versions = dict(versions or FIXTURE_PLUGINS)
    crates = tmp / "crates"
    for crate in versions:
        # Give every crate a binary target unless the case says otherwise: the
        # checker treats a plugin with no Rust crate as skill-only and exempts it
        # from provenance entirely, so a fixture that wrote no source would make
        # the provenance suite assert nothing.
        if crate in skill_only:
            continue
        main_rs = crates / crate / "src" / "main.rs"
        main_rs.parent.mkdir(parents=True, exist_ok=True)
        main_rs.write_text("fn main() {}\n", encoding="utf-8")
    for crate, ver in versions.items():
        if crate in drop_plugin_json:
            (crates / crate).mkdir(parents=True, exist_ok=True)
            continue
        pj = crates / crate / ".claude-plugin" / "plugin.json"
        if crate in corrupt_plugin_json:
            pj.parent.mkdir(parents=True, exist_ok=True)
            pj.write_text("{ this is not json", encoding="utf-8")
            continue
        if crate in omit_version:
            _write_json(pj, {"name": crate})
            continue
        _write_json(pj, {"name": crate, "version": ver})
    # A non-plugin crate must be skipped entirely by both dimensions.
    (crates / "harness-core").mkdir(parents=True, exist_ok=True)

    registry_path = tmp / "installed_plugins.json"
    # Each installed plugin gets a cache dir; the deployed binary's provenance
    # manifest lives beside the binary in there. `provenance` maps crate -> the
    # manifest dict to write (None = write no manifest at all, the "cannot tell
    # what this binary was built from" case).
    cache_root = tmp / "cache"
    if provenance is None:
        provenance = {}
    if registry_text is not None:
        registry_path.write_text(registry_text, encoding="utf-8")
    elif write_registry:
        rv = dict(versions) if registry_versions is None else dict(registry_versions)
        plugins = {}
        for c, v in rv.items():
            if v is None:
                continue
            entry = {"version": v}
            if install_paths:
                install = cache_root / c / str(v)
                install.mkdir(parents=True, exist_ok=True)
                entry["installPath"] = str(install)
                if c in force_host_binary or (
                    c not in skill_only and c not in no_host_binary
                ):
                    hostbin = install / "bin" / f"{c}-{_HOST_SUFFIX}"
                    hostbin.parent.mkdir(parents=True, exist_ok=True)
                    hostbin.write_text("", encoding="utf-8")
                manifest = provenance.get(c, _DEFAULT_PROVENANCE)
                if provenance_text is not None and c in provenance:
                    (install / ".deployed-from.json").write_text(
                        provenance_text, encoding="utf-8"
                    )
                elif manifest is not None:
                    _write_json(install / ".deployed-from.json", manifest)
            plugins[f"{c}@{OWNER}"] = [entry]
        for c in omit_version:
            plugins[f"{c}@{OWNER}"] = [{}]
        for c, raw in (registry_raw_entries or {}).items():
            plugins[f"{c}@{OWNER}"] = raw
        _write_json(registry_path, {"plugins": plugins})

    settings_path = tmp / "settings.json"
    if settings_text is not None:
        settings_path.write_text(settings_text, encoding="utf-8")
    elif write_settings:
        if enabled_plugins_raw is not None:
            _write_json(settings_path, {"enabledPlugins": enabled_plugins_raw})
        else:
            en = {c: True for c in versions}
            en.update(enabled or {})
            for c in enabled_absent:
                en.pop(c, None)
            _write_json(
                settings_path,
                {"enabledPlugins": {f"{c}@{OWNER}": v for c, v in en.items()}},
            )
    return crates, registry_path, settings_path


class _FixtureCase(unittest.TestCase):
    """Rebinds the script's path constants at the fixture, restores after."""

    def run_main(self, tmp, *, changed=(), **kwargs):
        crates, registry_path, settings_path = _make_fixture(Path(tmp), **kwargs)
        saved = (
            cpr.CRATES,
            cpr.REGISTRY_PATH,
            cpr.SETTINGS_PATH,
            getattr(cpr, "SOURCE_CHANGED_SINCE", None),
        )
        cpr.CRATES = str(crates)
        cpr.REGISTRY_PATH = str(registry_path)
        cpr.SETTINGS_PATH = str(settings_path)
        cpr.SOURCE_CHANGED_SINCE = _changed_stub(changed)
        out, err = io.StringIO(), io.StringIO()
        try:
            with redirect_stdout(out), redirect_stderr(err):
                rc = cpr.main()
        finally:
            (
                cpr.CRATES,
                cpr.REGISTRY_PATH,
                cpr.SETTINGS_PATH,
                cpr.SOURCE_CHANGED_SINCE,
            ) = saved
        return rc, out.getvalue(), err.getvalue()


class Enablement(_FixtureCase):
    def test_gate_crate_absent_from_enabled_plugins_fails(self):
        """The demonstrated hole: propguard installed at the right version, every
        version/rollout gate green, but absent from enabledPlugins -> inert. That
        must now be a hard failure, not a silent pass."""
        with tempfile.TemporaryDirectory() as tmp:
            rc, _out, err = self.run_main(tmp, enabled_absent=("propguard",))
            self.assertEqual(rc, cpr.RC_ENABLEMENT)
            self.assertIn("DISABLED OR UNVERIFIABLE GATE CRATE", err)
            self.assertIn("propguard", err)
            self.assertIn("absent from enabledPlugins", err)

    def test_gate_crate_explicitly_false_fails(self):
        """An explicit `false` is exactly as inert as an absent key."""
        with tempfile.TemporaryDirectory() as tmp:
            rc, _out, err = self.run_main(tmp, enabled={"blastguard": False})
            self.assertEqual(rc, cpr.RC_ENABLEMENT)
            self.assertIn("blastguard", err)
            self.assertIn("disabled (set to false)", err)

    def test_non_gate_disabled_is_a_warning_not_a_failure(self):
        """Users disable non-gate plugins on purpose (benchkit / daily-report /
        ship are off deliberately today) — inform, never block."""
        with tempfile.TemporaryDirectory() as tmp:
            rc, _out, err = self.run_main(tmp, enabled_absent=("benchkit",))
            self.assertEqual(rc, 0)
            self.assertIn("benchkit", err)
            self.assertNotIn("DISABLED OR UNVERIFIABLE GATE CRATE", err)

    def test_all_enabled_passes(self):
        with tempfile.TemporaryDirectory() as tmp:
            rc, out, err = self.run_main(tmp)
            self.assertEqual(rc, 0, err)
            self.assertIn("GATE plugin(s) enabled", out)

    def test_absent_settings_file_skips_the_dimension(self):
        """Fail-soft, exactly as an absent registry skips the rollout check: a
        machine with no settings.json is not a failure."""
        with tempfile.TemporaryDirectory() as tmp:
            rc, out, _err = self.run_main(tmp, write_settings=False)
            self.assertEqual(rc, 0)
            self.assertIn("SKIP: no settings", out)

    def test_absent_settings_does_not_mask_rollout_drift(self):
        """Skipping one dimension must not soften the other."""
        with tempfile.TemporaryDirectory() as tmp:
            rc, _out, err = self.run_main(
                tmp,
                write_settings=False,
                registry_versions={**FIXTURE_PLUGINS, "condukt": "2.0.0"},
            )
            self.assertEqual(rc, 1)
            self.assertIn("ROLLOUT DRIFT", err)

    def test_disabled_gate_fails_even_when_rollout_is_clean(self):
        """The whole point: version + rollout can be perfectly green while the
        gate is inert. Enablement is an independent dimension."""
        with tempfile.TemporaryDirectory() as tmp:
            rc, out, err = self.run_main(tmp, enabled_absent=("propguard",))
            self.assertEqual(rc, cpr.RC_ENABLEMENT)
            self.assertIn("no rollout drift", out)  # rollout dimension passed
            self.assertIn("DISABLED OR UNVERIFIABLE GATE CRATE", err)


class RolloutDrift(_FixtureCase):
    """Pre-existing behaviour — guarded so the new dimension doesn't regress it."""

    def test_stale_registry_version_is_detected(self):
        with tempfile.TemporaryDirectory() as tmp:
            rc, _out, err = self.run_main(
                tmp, registry_versions={**FIXTURE_PLUGINS, "blastguard": "1.1.0"}
            )
            self.assertEqual(rc, 1)
            self.assertIn("ROLLOUT DRIFT", err)
            self.assertIn("source=1.2.0 registry=1.1.0", err)
            self.assertIn("rollout-plugins.sh not run since bump", err)

    def test_never_installed_plugin_is_detected(self):
        with tempfile.TemporaryDirectory() as tmp:
            rc, _out, err = self.run_main(
                tmp, registry_versions={**FIXTURE_PLUGINS, "condukt": None}
            )
            self.assertEqual(rc, 1)
            self.assertIn("never installed", err)

    def test_absent_registry_skips_the_dimension(self):
        with tempfile.TemporaryDirectory() as tmp:
            rc, out, _err = self.run_main(tmp, write_registry=False)
            self.assertEqual(rc, 0)
            self.assertIn("SKIP: no registry", out)

    def test_clean_fixture_passes(self):
        with tempfile.TemporaryDirectory() as tmp:
            rc, out, err = self.run_main(tmp)
            self.assertEqual(rc, 0, err)
            self.assertIn("no rollout drift", out)

    def test_non_plugin_crate_is_skipped(self):
        """crates/harness-core has no plugin.json and must not be counted."""
        with tempfile.TemporaryDirectory() as tmp:
            rc, out, err = self.run_main(tmp)
            self.assertEqual(rc, 0, err)
            self.assertIn(f"{len(FIXTURE_PLUGINS)} plugins deployed", out)


class MalformedInputs(_FixtureCase):
    """A PRESENT-but-unparseable file is not the same as an absent one. Both used
    to collapse to None, so a corrupt file printed "not found: <path>" (a lie)
    and SKIPped with rc=0 — fail-open on a state where nothing is enabled."""

    def test_malformed_settings_is_a_hard_failure_not_a_skip(self):
        with tempfile.TemporaryDirectory() as tmp:
            rc, out, err = self.run_main(tmp, settings_text="{ oops, not json")
            self.assertEqual(rc, cpr.RC_ENABLEMENT)
            self.assertIn("present but unparseable", err)
            self.assertNotIn("SKIP: no settings", out)
            self.assertNotIn("settings.json not found", err)

    def test_malformed_registry_is_a_hard_failure_not_a_skip(self):
        with tempfile.TemporaryDirectory() as tmp:
            rc, out, err = self.run_main(tmp, registry_text="[[[")
            self.assertEqual(rc, cpr.RC_ROLLOUT)
            self.assertIn("present but unparseable", err)
            self.assertNotIn("SKIP: no registry", out)
            self.assertNotIn("installed_plugins.json not found", err)

    def test_non_dict_enabled_plugins_is_a_failure_not_a_traceback(self):
        """A JSON list for enabledPlugins used to raise AttributeError on
        `.get`. It must be a diagnosable failure, and a FAILURE rather than
        "nothing enabled": Claude Code cannot honour the mistyped value, so
        every gate is inert — the exact fail-open this dimension exists for."""
        with tempfile.TemporaryDirectory() as tmp:
            rc, _out, err = self.run_main(
                tmp, enabled_plugins_raw=["blastguard@yukineko"]
            )
            self.assertEqual(rc, cpr.RC_ENABLEMENT)
            self.assertIn("enabledPlugins", err)
            self.assertIn("not an object", err)

    def test_absent_enabled_plugins_key_is_not_a_shape_failure(self):
        """An omitted enabledPlugins is a legitimate (if inert) config, and it
        already fails loudly per-gate; it must not ALSO be reported as a shape
        error, which would misdescribe the problem."""
        with tempfile.TemporaryDirectory() as tmp:
            rc, _out, err = self.run_main(tmp, settings_text="{}")
            self.assertEqual(rc, cpr.RC_ENABLEMENT)
            self.assertNotIn("not an object", err)
            self.assertIn("absent from enabledPlugins", err)


class GateReconciliation(_FixtureCase):
    """gates_seen used to be derived from the plugins actually FOUND, so a GATE
    plugin whose plugin.json was deleted/renamed/corrupted was neither checked
    nor missed — it vanished and the run printed a green "all N enabled"."""

    def test_gate_with_deleted_plugin_json_is_reported(self):
        # Reported under RC_UNVERIFIABLE rather than RC_ENABLEMENT: the fix is to
        # restore the plugin.json, and pre-push's rc=2 branch asserts the remedy
        # is an enabledPlugins edit, which would be wrong here.
        with tempfile.TemporaryDirectory() as tmp:
            rc, out, err = self.run_main(tmp, drop_plugin_json=("specguard",))
            self.assertEqual(rc, cpr.RC_UNVERIFIABLE)
            self.assertIn("specguard", err)
            self.assertIn("no readable plugin.json", err)
            self.assertNotIn("GATE plugin(s) enabled", out)

    def test_gate_with_corrupt_plugin_json_is_reported(self):
        with tempfile.TemporaryDirectory() as tmp:
            rc, _out, err = self.run_main(tmp, corrupt_plugin_json=("overwatch",))
            self.assertEqual(rc, cpr.RC_UNVERIFIABLE)
            self.assertIn("overwatch", err)
            self.assertIn("present but unparseable", err)

    def test_renamed_gate_crate_dir_is_accounted_for_by_plugin_name(self):
        """Reconciliation matches on EITHER the crate dir or the plugin name, so
        moving crates/specguard -> crates/spec-guard while keeping the plugin
        name is not a false alarm."""
        versions = {k: v for k, v in FIXTURE_PLUGINS.items() if k != "specguard"}
        with tempfile.TemporaryDirectory() as tmp:
            crates = Path(tmp) / "crates"
            rc, _out, err = self.run_main(tmp, versions=versions)
            self.assertIn("specguard", err)  # baseline: missing gate is caught
            _write_json(
                crates / "spec-guard" / ".claude-plugin" / "plugin.json",
                {"name": "specguard", "version": "2.1.0"},
            )
            # Re-run against the same tree, now with the renamed dir present.
            saved = (cpr.CRATES, cpr.REGISTRY_PATH, cpr.SETTINGS_PATH)
            cpr.CRATES = str(crates)
            cpr.REGISTRY_PATH = str(Path(tmp) / "installed_plugins.json")
            cpr.SETTINGS_PATH = str(Path(tmp) / "settings.json")
            err2 = io.StringIO()
            try:
                with redirect_stdout(io.StringIO()), redirect_stderr(err2):
                    cpr.main()
            finally:
                cpr.CRATES, cpr.REGISTRY_PATH, cpr.SETTINGS_PATH = saved
            self.assertNotIn("no readable plugin.json", err2.getvalue())

    def test_mutategate_is_an_explicit_exemption(self):
        """mutategate ships no plugin.json by design and must never be demanded
        of the scan — otherwise the clean fixture could not pass at all."""
        self.assertNotIn("mutategate", cpr.EXPECTED_GATE_PLUGINS)
        self.assertIn("mutategate", cpr.GATE_CRATES)
        self.assertIn("mutategate", cpr.NON_PLUGIN_GATES)
        self.assertEqual(
            set(cpr.EXPECTED_GATE_PLUGINS),
            set(cpr.GATE_CRATES) - set(cpr.NON_PLUGIN_GATES),
        )


class UnverifiablePlugins(_FixtureCase):
    """A plugin.json the checker cannot read is a plugin it cannot verify on
    EITHER dimension. Skipping it silently shrinks the denominator of both green
    lines, so the run reports a smaller success and nothing points at the gap.
    """

    def _mentions(self, out, err, needle):
        return needle in out or needle in err

    def test_corrupt_gate_plugin_json_with_absent_settings_is_not_silent(self):
        """F3. The reconciliation that catches an unreadable GATE plugin.json
        used to live INSIDE check_enabled(), after its early return for an absent
        settings.json — so on a machine with no settings.json it never ran at
        all. rc was 0 and the crate name appeared NOWHERE in the output; even the
        count line was suppressed, so there was not even a shrinking number to
        notice."""
        with tempfile.TemporaryDirectory() as tmp:
            rc, out, err = self.run_main(
                tmp, corrupt_plugin_json=("overwatch",), write_settings=False
            )
            self.assertNotEqual(rc, 0, f"silent pass\nSTDOUT:{out}\nSTDERR:{err}")
            self.assertTrue(
                self._mentions(out, err, "overwatch"),
                f"the unverifiable GATE crate is never named\nSTDOUT:{out}\nSTDERR:{err}",
            )
            self.assertIn("no readable plugin.json", err)

    def test_deleted_gate_plugin_json_with_absent_settings_is_not_silent(self):
        """Same hole reached by deleting the file rather than corrupting it."""
        with tempfile.TemporaryDirectory() as tmp:
            rc, out, err = self.run_main(
                tmp, drop_plugin_json=("specguard",), write_settings=False
            )
            self.assertNotEqual(rc, 0, f"silent pass\nSTDOUT:{out}\nSTDERR:{err}")
            self.assertTrue(self._mentions(out, err, "specguard"))
            self.assertIn("no readable plugin.json", err)

    def test_non_gate_corrupt_plugin_json_is_rollout_checked_or_reported(self):
        """F4. A NON-gate plugin with a corrupt plugin.json was dropped from the
        scan entirely: rc=0, "OK: 5 plugins deployed at their source version",
        and `condukt` mentioned nowhere. The denominator silently went 6 -> 5,
        the same shrinking-denominator failure the enablement dimension was
        already hardened against."""
        with tempfile.TemporaryDirectory() as tmp:
            rc, out, err = self.run_main(tmp, corrupt_plugin_json=("condukt",))
            self.assertNotEqual(rc, 0, f"silent pass\nSTDOUT:{out}\nSTDERR:{err}")
            self.assertTrue(
                self._mentions(out, err, "condukt"),
                f"the unverifiable plugin is never named\nSTDOUT:{out}\nSTDERR:{err}",
            )

    def test_unverifiable_plugin_suppresses_the_green_rollout_line(self):
        """The green line must not claim a clean rollout over a denominator that
        silently lost a plugin. "OK: 5 plugins deployed" next to an unreadable
        6th is the false confidence this whole class of defect produces."""
        with tempfile.TemporaryDirectory() as tmp:
            _rc, out, _err = self.run_main(tmp, corrupt_plugin_json=("condukt",))
            self.assertNotIn("plugins deployed at their source version", out)

    def test_unverifiable_gate_suppresses_the_green_enablement_line(self):
        with tempfile.TemporaryDirectory() as tmp:
            _rc, out, _err = self.run_main(tmp, corrupt_plugin_json=("overwatch",))
            self.assertNotIn("GATE plugin(s) enabled", out)


class MalformedPluginJsonFields(_FixtureCase):
    def test_missing_source_version_is_not_counted_as_deployed(self):
        """F5. `src_ver` came straight from `pjd.get("version")` with no presence
        check, and the registry side can be None too (entry `[{}]`). `None !=
        None` is False, so a plugin with NO version on either side counted as
        cleanly deployed and the run printed OK."""
        with tempfile.TemporaryDirectory() as tmp:
            rc, out, err = self.run_main(tmp, omit_version=("condukt",))
            self.assertNotEqual(rc, 0, f"silent pass\nSTDOUT:{out}\nSTDERR:{err}")
            self.assertIn("condukt", out + err)
            self.assertNotIn("plugins deployed at their source version", out)

    def test_non_dict_registry_entry_is_a_diagnosable_failure_not_a_traceback(self):
        """F6. `isinstance(entry, list) and entry` guards the LIST, not its
        elements, so `["1.0.0"]` reached `.get` and raised AttributeError. The
        traceback exited 1 — which collides with RC_ROLLOUT, so pre-push routed a
        crash straight to the rollout remediation."""
        with tempfile.TemporaryDirectory() as tmp:
            rc, out, err = self.run_main(
                tmp, registry_raw_entries={"blastguard": ["1.2.0"]}
            )
            self.assertNotIn("Traceback", err)
            self.assertEqual(rc, cpr.RC_ROLLOUT, f"STDOUT:{out}\nSTDERR:{err}")
            self.assertIn("blastguard", err)

    def test_other_malformed_registry_entry_shapes_do_not_crash(self):
        for raw in ([], {}, "1.2.0", None, [None]):
            with self.subTest(raw=raw), tempfile.TemporaryDirectory() as tmp:
                rc, out, err = self.run_main(
                    tmp, registry_raw_entries={"blastguard": raw}
                )
                self.assertNotIn("Traceback", err)
                self.assertNotEqual(rc, 0, f"STDOUT:{out}\nSTDERR:{err}")
                self.assertIn("blastguard", err)


class HintGeneration(unittest.TestCase):
    def test_hint_is_generated_from_the_single_constant(self):
        """Task B: the file holds ONE copy of the GATE list. The hint prose must
        be derived from it, so it cannot drift from the constant."""
        hint = cpr.rollout_hint()
        for crate in cpr.GATE_CRATES:
            self.assertIn(crate, hint)
        self.assertIn("--canary for GATE crates:", hint)
        self.assertIn("/".join(cpr.GATE_CRATES), hint)

    def test_no_second_literal_copy_of_the_gate_list_in_the_source(self):
        """Guard against someone re-introducing a hardcoded list in the prose.

        Asserting only `assertNotIn("blastguard/propguard", src)` pins ONE
        spelling of ONE ordering: a re-introduced literal that is reordered
        ("propguard/blastguard"), partial, or differently separated slips
        straight past it. Instead, scan every line OUTSIDE the single
        module-level GATE_CRATES tuple for the two SHAPES a duplicated list
        actually takes, in any order:
          - two or more QUOTED gate names (a re-introduced tuple/list literal);
          - two gate names joined by `/` (the hint's own separator).
        Narrative prose that merely happens to name two gates in a sentence
        (e.g. the incident write-up in the module docstring) is neither, and is
        correctly not flagged.
        """
        import re
        src = (_HERE / "check-plugin-rollout.py").read_text(encoding="utf-8")
        lines = src.splitlines()

        # Locate the one legitimate copy: the module-level tuple, from its
        # `GATE_CRATES = (` line to the closing `)`, so it is exempt below.
        start = next(
            i for i, ln in enumerate(lines) if ln.startswith("GATE_CRATES = (")
        )
        end = next(i for i in range(start, len(lines)) if lines[i].startswith(")"))
        exempt = set(range(start, end + 1))
        # NON_PLUGIN_GATES/EXPECTED_GATE_PLUGINS derive from it rather than
        # repeating it, and name at most one crate each — no exemption needed.

        gate_alt = "|".join(re.escape(c) for c in cpr.GATE_CRATES)
        quoted = re.compile(rf"""["']({gate_alt})["']""")
        slash_joined = re.compile(rf"\b({gate_alt})\s*/\s*({gate_alt})\b")

        offenders = []
        for i, line in enumerate(lines):
            if i in exempt:
                continue
            if len(set(quoted.findall(line))) >= 2 or slash_joined.search(line):
                offenders.append((i + 1, line.strip()))
        self.assertEqual(
            offenders,
            [],
            "a second copy of the GATE list appears to have been re-introduced "
            f"outside the module-level constant: {offenders}",
        )
        # And the constant really is the only line-anchored definition.
        self.assertEqual(
            sum(1 for ln in lines if ln.startswith("GATE_CRATES = (")), 1
        )


class BinaryProvenance(_FixtureCase):
    """A matching version string is not evidence that the deployed binary is current.

    The registry records a version, and rollout-plugins.sh only re-points that
    entry when the version CHANGES — but rebuild-plugins.sh refreshes the binary
    whenever the bytes differ. So a plugin can sit at the right version with a
    binary built from source that has since moved (most commonly because
    harness-core changed underneath it: every plugin statically links it, and
    none of them bump for it). Comparing version strings cannot see that, and
    comparing binary HASHES cannot either — this repo's release builds are NOT
    byte-reproducible (measured 2026-07-26: `cargo clean -p X && cargo build`
    yields a different sha256 for identical source). What IS decidable is
    provenance: record the commit each binary was built from, then ask git
    whether that plugin's source moved since.
    """

    def test_stale_binary_at_matching_version_is_drift(self):
        # Every version string matches, so the old check reports a clean fleet.
        # But condukt's source has moved since its binary was built.
        with tempfile.TemporaryDirectory() as tmp:
            rc, out, err = self.run_main(
                tmp,
                provenance={"condukt": {"commit": "deadbeef" * 5, "dirty": False}},
                changed=["condukt"],
            )
        self.assertNotEqual(
            rc, 0, f"a stale binary must not pass as deployed.\nout={out}\nerr={err}"
        )
        self.assertIn("condukt", out + err)

    def test_clean_provenance_still_passes(self):
        # Control arm (anti-vacuity): if every binary was built from source that
        # has not moved, the checker must still report a clean fleet. Without
        # this, the assertions above would also hold for a checker that simply
        # fails on everything.
        with tempfile.TemporaryDirectory() as tmp:
            rc, out, err = self.run_main(tmp)
        self.assertEqual(rc, 0, f"clean fleet must pass.\nout={out}\nerr={err}")
        self.assertIn("no rollout drift", out)

    def test_missing_provenance_manifest_is_drift_not_clean(self):
        # No manifest = the checker cannot tell what the binary was built from.
        # "Cannot determine" must resolve restrictively, not to "fine".
        with tempfile.TemporaryDirectory() as tmp:
            rc, out, err = self.run_main(tmp, provenance={"blastguard": None})
        self.assertNotEqual(
            rc, 0, f"unverifiable provenance must not pass.\nout={out}\nerr={err}"
        )
        self.assertIn("blastguard", out + err)

    def test_binary_built_from_dirty_tree_is_drift(self):
        # Built from uncommitted work: the recorded commit does not describe the
        # bytes, so no later comparison against it can be trusted.
        with tempfile.TemporaryDirectory() as tmp:
            rc, out, err = self.run_main(
                tmp,
                provenance={"specguard": {"commit": "abc123" * 6, "dirty": True}},
            )
        self.assertNotEqual(
            rc, 0, f"a binary built from a dirty tree must not pass.\nout={out}\nerr={err}"
        )
        self.assertIn("specguard", out + err)

    def test_undeterminable_git_answer_is_drift(self):
        # git could not answer (commit rebased away, repo unavailable). That is
        # not "unchanged".
        with tempfile.TemporaryDirectory() as tmp:
            rc, out, err = self.run_main(
                tmp,
                provenance={"propguard": {"commit": "0" * 40, "dirty": False}},
                changed={"propguard": None},
            )
        self.assertNotEqual(
            rc, 0, f"an undeterminable provenance answer must not pass.\nout={out}\nerr={err}"
        )
        self.assertIn("propguard", out + err)

    def test_malformed_provenance_manifest_is_drift(self):
        with tempfile.TemporaryDirectory() as tmp:
            rc, out, err = self.run_main(
                tmp,
                provenance={"overwatch": {"commit": "x" * 40, "dirty": False}},
                provenance_text="{ not json",
            )
        self.assertNotEqual(
            rc, 0, f"a malformed manifest must not pass.\nout={out}\nerr={err}"
        )
        self.assertIn("overwatch", out + err)


class SkillOnlyPlugins(_FixtureCase):
    """Which plugins the provenance dimension applies to at all.

    Not every plugin compiles to a binary: daily-report and scout are skills and
    hooks only, with no crates/<name> Rust crate. Demanding a provenance manifest
    from them reported permanent, unfixable drift — no rollout can ever write a
    manifest beside a binary that does not exist, so the check would have stayed
    red forever and trained its reader to ignore it.

    The exemption is narrow on purpose, and these tests pin both edges of it: it
    is decided from the SOURCE, and it does not survive a binary actually being
    on disk. "bin/ is missing" must never be self-certifying, because that is
    itself a real failure — a fresh version dir with no host binary makes the
    launcher exec nothing and silently no-op.
    """

    def test_skill_only_plugin_needs_no_provenance(self):
        """A plugin with no Rust crate has no binary whose provenance can drift."""
        with tempfile.TemporaryDirectory() as tmp:
            rc, out, err = self.run_main(
                tmp,
                skill_only=("benchkit",),
                provenance={"benchkit": None},
            )
        self.assertEqual(
            rc, 0, f"a skill-only plugin must not be reported as drift.\nout={out}\nerr={err}"
        )

    def test_skill_only_exemption_does_not_excuse_binary_plugins(self):
        """Control arm: the exemption must not blanket-pass the other plugins.

        Without this, `_crate_ships_binary` returning False for everything would
        satisfy the test above while silently disabling the entire dimension.
        """
        with tempfile.TemporaryDirectory() as tmp:
            rc, out, err = self.run_main(
                tmp,
                skill_only=("benchkit",),
                provenance={"benchkit": None, "condukt": None},
            )
        self.assertNotEqual(
            rc, 0,
            f"a binary plugin with no manifest must still be drift.\nout={out}\nerr={err}",
        )
        self.assertIn("condukt", out + err)
        self.assertNotIn("benchkit", out + err)

    def test_declared_binary_not_deployed_is_drift(self):
        """Source declares a binary but none is deployed: execs nothing.

        specguard keeps a perfectly clean manifest here, so the only thing wrong
        is the absent binary — which is exactly the state a manifest-only check
        waves through.
        """
        with tempfile.TemporaryDirectory() as tmp:
            rc, out, err = self.run_main(
                tmp,
                no_host_binary=("specguard",),
            )
        self.assertNotEqual(
            rc, 0,
            f"a declared-but-undeployed binary must not pass.\nout={out}\nerr={err}",
        )
        self.assertIn("specguard", out + err)

    def test_deployed_binary_still_needs_provenance_even_if_source_has_no_crate(self):
        """A binary on disk must be verifiable whatever the source says.

        The exemption keys off the source, so a stale binary left behind after
        its crate was removed would otherwise be waved through unverified.
        """
        with tempfile.TemporaryDirectory() as tmp:
            rc, out, err = self.run_main(
                tmp,
                skill_only=("benchkit",),
                provenance={"benchkit": None},
                force_host_binary=("benchkit",),
            )
        self.assertNotEqual(
            rc, 0,
            f"a deployed binary with no manifest must not pass.\nout={out}\nerr={err}",
        )
        self.assertIn("benchkit", out + err)


if __name__ == "__main__":
    unittest.main()
