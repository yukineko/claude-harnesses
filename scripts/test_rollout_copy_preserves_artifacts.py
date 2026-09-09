#!/usr/bin/env python3
"""Regression test for `copy_plugin_dir` in scripts/rollout-plugins.sh.

Stdlib-only (`unittest`), no network, no cargo. Never touches the real plugin
cache (`~/.claude/plugins/cache/...`): every case runs against throwaway temp
dirs.

WHAT IS UNDER TEST
  `copy_plugin_dir <src-crate-dir> <deployed-version-dir>` mirrors a crate over
  an ALREADY-DEPLOYED cache version dir. That destination is not a pristine copy
  of the crate: scripts/rebuild-plugins.sh writes two kinds of generated file
  into it AFTER the copy step --

    * `bin/<name>-<os>-<arch>`  the per-platform binary the launcher execs
    * `.deployed-from.json`     the provenance manifest

  Neither exists in the crate (`git ls-files crates/overwatch/bin/` lists only
  the launcher `crates/overwatch/bin/overwatch`), and
  scripts/check-plugin-rollout.py already encodes that contract: its
  `_is_rebuild_artifact()` (~line 904) admits exactly `bin/<name>-<suffix>` for
  the suffixes in its `PLATFORM_SUFFIXES` table (~line 200) plus the provenance
  file, as files legitimately present in the deployed tree with no crate
  counterpart.

  A re-copy over such a dir must therefore NOT destroy `bin/<name>-<suffix>`.
  If it does, the plugin is left "deployed" carrying only its launcher shell
  script; the launcher then cannot exec anything and the plugin's hooks do not
  run at all. That failure is DARK, not red -- nothing reports a finding,
  because the gate that would report it never starts.

WHY THE FUNCTION IS EXTRACTED RATHER THAN SOURCED
  scripts/rollout-plugins.sh is not source-safe. Below its function definitions
  it runs top-level code under `set -euo pipefail`: `cd "$(dirname "$0")/.."`,
  it resolves `$HOME/.claude/plugins/...` paths, `exit 1`s when the live
  registry is missing, shells out to `git rev-parse HEAD`, parses the real
  marketplace.json and -- absent --dry-run -- performs an actual rollout against
  the running harness. Sourcing it from a test would mutate the developer's
  live plugin installation. So the single function under test is sliced out
  verbatim with `sed -n '/^copy_plugin_dir()/,/^}/p'` and sourced in isolation.
  `_extract_copy_plugin_dir` asserts the slice is well-formed rather than
  silently testing an empty string: an extraction that matched nothing would
  otherwise make every assertion below vacuous (a test that verifies nothing
  passes for free).

BOTH BRANCHES ARE COVERED
  The function has an rsync path and a `cp -a` fallback selected by
  `command -v rsync`. The fallback is forced by running under a PATH that
  mirrors the real one with `rsync` alone withheld; `_assert_branch` then probes
  which way `command -v rsync` actually resolves under each PATH, so a case can
  never claim to cover a branch it did not enter.

RUNNING THIS SUITE AGAINST AN OLDER `copy_plugin_dir` (the F->P oracle)
  `ROLLOUT_SH_UNDER_TEST=<path>` points the extraction at some other file that
  contains a `copy_plugin_dir()` definition instead of scripts/rollout-plugins.sh.
  Its only purpose is to let a reviewer observe these cases FAILING against a
  prior revision of the function -- e.g.

      git show <rev>:scripts/rollout-plugins.sh \\
        | sed -n '/^copy_plugin_dir()/,/^}/p' > /tmp/old.bash
      ROLLOUT_SH_UNDER_TEST=/tmp/old.bash python3 <this file>

  -- because a test that has never been seen to fail has not been shown to
  observe anything (CLAUDE.md 2(b)). The override is read only here, defaults to
  the real script, and the same `_extract_copy_plugin_dir` well-formedness
  assertions apply to it, so a typo'd path is a loud error rather than a
  vacuous pass.
"""
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_SCRIPT = Path(os.environ.get("ROLLOUT_SH_UNDER_TEST")
               or (_HERE / "rollout-plugins.sh")).resolve()

PLUGIN = "overwatch"

# A binary the crate USED to ship and no longer does (renamed, or dropped). Its
# `bin/<stem>-<suffix>` in the deployed dir is an orphan: rebuild-plugins.sh
# builds from the crate, so nothing will ever write to it again.
ORPHAN_STEM = "overwatch-legacy"

# Mirrors scripts/check-plugin-rollout.py's PLATFORM_SUFFIXES (the canonical
# list). Restated here rather than imported: this test asserts a contract
# BETWEEN two scripts, and a fixture that borrowed the checker's own table would
# be unable to fail when only rollout-plugins.sh is wrong about the shape.
PLATFORM_SUFFIXES = (
    "darwin-arm64",
    "darwin-x86_64",
    "linux-x86_64",
    "linux-arm64",
    "windows-x86_64",
    "windows-arm64",
    "windows-x86_64.exe",
    "windows-arm64.exe",
)

# Utilities today's cp -a fallback invokes by name. `find` execs `rm`, so rm has
# to be reachable too. This is only a sanity floor on the shim PATH below, NOT
# the set of tools a fix is allowed to use.
FALLBACK_UTILS = ("mkdir", "find", "cp", "rm")

_SHIM_TMP = None

# Resolved once against the REAL PATH and then always invoked absolutely: the
# shim PATH below deliberately omits rsync, and subprocess resolves the program
# name against the env it is handed, so a bare "bash" would fail to launch at
# all and turn a branch this suite must observe into a setup error.
_BASH = shutil.which("bash")


def _extract_copy_plugin_dir():
    """Return the verbatim source of copy_plugin_dir(), or fail loudly."""
    text = _SCRIPT.read_text(encoding="utf-8")
    starts = [ln for ln in text.splitlines() if ln.startswith("copy_plugin_dir()")]
    if len(starts) != 1:
        raise AssertionError(
            f"expected exactly 1 top-level `copy_plugin_dir()` definition in "
            f"{_SCRIPT}, found {len(starts)} -- the extraction below would be "
            f"ambiguous, so no assertion made against it could be trusted"
        )
    out = subprocess.run(
        ["sed", "-n", "/^copy_plugin_dir()/,/^}/p", str(_SCRIPT)],
        capture_output=True, text=True, check=True,
    ).stdout
    if "rsync" not in out or not out.rstrip().endswith("}"):
        raise AssertionError(
            f"extracted slice does not look like the whole function:\n{out!r}"
        )
    return out


class CopyPluginDirCase(unittest.TestCase):
    """Shared fixture: an extracted function, a fake crate, a deployed dir."""

    @classmethod
    def setUpClass(cls):
        if _BASH is None:
            raise AssertionError("bash is not on PATH; cannot run the function "
                                 "under test at all")
        cls.func_src = _extract_copy_plugin_dir()

    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = Path(self._tmp.name)
        self.addCleanup(self._tmp.cleanup)
        self.funcfile = self.tmp / "copy_plugin_dir.bash"
        self.funcfile.write_text(self.func_src, encoding="utf-8")

    # --- fixtures ---------------------------------------------------------
    def _make_src_crate(self):
        """A crate as git tracks it: launcher only, NO per-platform binary."""
        src = self.tmp / "crate"
        (src / "bin").mkdir(parents=True)
        (src / "bin" / PLUGIN).write_text(
            '#!/bin/sh\nexec "$0-$(uname)"\n', encoding="utf-8")
        (src / "bin" / PLUGIN).chmod(0o755)
        (src / ".claude-plugin").mkdir()
        (src / ".claude-plugin" / "plugin.json").write_text(
            '{"name": "overwatch", "version": "1.2.3"}\n', encoding="utf-8")
        (src / "hooks").mkdir()
        (src / "hooks" / "session-start.sh").write_text(
            "#!/bin/sh\n:\n", encoding="utf-8")
        return src

    def _make_deployed(self, suffixes=("linux-x86_64",)):
        """A deployed version dir as it looks AFTER rebuild-plugins.sh ran.

        Carries the crate's own files, the generated per-platform binaries, and
        one stray file that is NOT a rebuild artifact -- the control that keeps
        this test from passing under a `copy_plugin_dir` that simply stopped
        deleting anything.
        """
        dst = self.tmp / "deployed"
        (dst / "bin").mkdir(parents=True)
        (dst / "bin" / PLUGIN).write_text(
            "#!/bin/sh\n# stale launcher\n", encoding="utf-8")
        for suf in suffixes:
            binary = dst / "bin" / f"{PLUGIN}-{suf}"
            binary.write_bytes(b"\x7fELF fake binary for " + suf.encode())
            binary.chmod(0o755)
        (dst / "stray-not-an-artifact.txt").write_text(
            "left over from an older version; must be deleted\n", encoding="utf-8")
        return dst

    def _plant_orphan(self, dst, suffixes=("linux-x86_64",)):
        """Leftovers of a binary the SOURCE CRATE no longer ships.

        Writes both halves of what a rename/removal strands in the cache: the
        stale launcher `bin/<stem>` and the per-platform artifact(s)
        `bin/<stem>-<suffix>`. The launcher is planted deliberately -- it is the
        only thing that distinguishes "is this stem still shipped?" asked of the
        SOURCE (correct) from the same question asked of the DESTINATION (which
        would answer yes for every orphan and protect it forever).

        Returns the artifact paths.
        """
        launcher = dst / "bin" / ORPHAN_STEM
        launcher.write_text('#!/bin/sh\nexec "$0-$(uname)"\n', encoding="utf-8")
        launcher.chmod(0o755)
        planted = []
        for suf in suffixes:
            binary = dst / "bin" / f"{ORPHAN_STEM}-{suf}"
            binary.write_bytes(b"\x7fELF orphaned binary for " + suf.encode())
            binary.chmod(0o755)
            planted.append(binary)
        crate_bin = self.tmp / "crate" / "bin"
        assert crate_bin.is_dir(), (
            "call _make_src_crate() before _plant_orphan(): with no crate to "
            "compare against, the check below cannot tell an orphan from a "
            "live artifact and would pass vacuously"
        )
        assert not (crate_bin / ORPHAN_STEM).exists(), (
            "fixture is self-contradictory: the crate ships a launcher for the "
            "stem this helper calls orphaned, so nothing here is an orphan"
        )
        return planted

    # --- invocation -------------------------------------------------------
    def _shim_path(self):
        """The real PATH with `rsync` — and only `rsync` — subtracted.

        Every PATH dir that holds an `rsync` is replaced by a symlink mirror of
        itself without that one entry; every other dir is passed through
        untouched. Subtracting rather than whitelisting keeps the fallback
        branch honestly reachable whatever tools a future `copy_plugin_dir`
        reaches for: a whitelist would turn a fix that merely used, say,
        `mktemp` into a red test — a verdict about the fixture, not the code.

        The mirrors are built once per process and cached.
        """
        global _SHIM_TMP
        if _SHIM_TMP is None:
            tmp = tempfile.TemporaryDirectory(prefix="rsyncless-path-")
            root = Path(tmp.name)
            parts = []
            for i, d in enumerate(os.environ.get("PATH", "").split(os.pathsep)):
                if not d or not os.path.isdir(d):
                    continue
                if not os.path.exists(os.path.join(d, "rsync")):
                    parts.append(d)
                    continue
                mirror = root / f"d{i}"
                mirror.mkdir()
                for name in os.listdir(d):
                    if name == "rsync":  # the one entry deliberately withheld
                        continue
                    try:
                        (mirror / name).symlink_to(os.path.join(d, name))
                    except OSError:
                        pass
                parts.append(str(mirror))
            _SHIM_TMP = (tmp, os.pathsep.join(parts))
        path = _SHIM_TMP[1]
        missing = [u for u in FALLBACK_UTILS
                   if shutil.which(u, path=path) is None]
        if missing:
            raise AssertionError(
                f"cannot exercise the cp -a fallback: {missing} absent from the "
                f"rsync-subtracted PATH"
            )
        return path

    def _assert_branch(self, env, want_rsync):
        """Observe which branch `command -v rsync` selects under this env."""
        rc = subprocess.run(
            [_BASH, "-c", "command -v rsync >/dev/null 2>&1"], env=env
        ).returncode
        has_rsync = rc == 0
        if has_rsync != want_rsync:
            self.fail(
                f"cannot honestly cover the "
                f"{'rsync' if want_rsync else 'cp -a fallback'} branch: "
                f"`command -v rsync` returned {rc} under PATH={env['PATH']!r} "
                f"(wanted {'found' if want_rsync else 'not found'}). This is a "
                f"setup problem, not a verdict about copy_plugin_dir."
            )

    def _run_copy(self, src, dst, *, use_rsync):
        env = dict(os.environ)
        if use_rsync:
            env["PATH"] = os.environ.get("PATH", "")
        else:
            env["PATH"] = self._shim_path()
        self._assert_branch(env, use_rsync)
        proc = subprocess.run(
            [_BASH, "-c",
             'set -euo pipefail; . "$1"; copy_plugin_dir "$2" "$3"',
             "_", str(self.funcfile), str(src), str(dst)],
            env=env, capture_output=True, text=True,
        )
        self.assertEqual(
            proc.returncode, 0,
            f"copy_plugin_dir exited {proc.returncode}\n"
            f"stdout: {proc.stdout}\nstderr: {proc.stderr}",
        )
        return proc


class TestRebuildArtifactsSurviveRecopy(CopyPluginDirCase):
    """The defect: a re-copy deletes the per-platform binary from the cache."""

    def _assert_binary_survived(self, dst, suffix):
        binary = dst / "bin" / f"{PLUGIN}-{suffix}"
        surviving = (sorted(p.name for p in (dst / "bin").iterdir())
                     if (dst / "bin").is_dir() else [])
        self.assertTrue(
            binary.is_file(),
            f"copy_plugin_dir deleted the rebuild artifact bin/{PLUGIN}-{suffix} "
            f"from the deployed dir. Surviving bin/ entries: {surviving}. "
            f"rebuild-plugins.sh only swaps binaries INTO existing cache bin "
            f"files, so nothing recreates it -- the plugin stays deployed with "
            f"its launcher alone and its hooks never run (dark, not red).",
        )

    def test_rsync_branch_preserves_per_platform_binary(self):
        src, dst = self._make_src_crate(), self._make_deployed()
        self._run_copy(src, dst, use_rsync=True)
        self._assert_binary_survived(dst, "linux-x86_64")

    def test_fallback_branch_preserves_per_platform_binary(self):
        src, dst = self._make_src_crate(), self._make_deployed()
        self._run_copy(src, dst, use_rsync=False)
        self._assert_binary_survived(dst, "linux-x86_64")

    def test_rsync_branch_preserves_every_platform_suffix(self):
        """Every suffix check-plugin-rollout.py calls a rebuild artifact,
        including the `.exe` forms cargo emits on Windows."""
        src = self._make_src_crate()
        dst = self._make_deployed(suffixes=PLATFORM_SUFFIXES)
        self._run_copy(src, dst, use_rsync=True)
        missing = [s for s in PLATFORM_SUFFIXES
                   if not (dst / "bin" / f"{PLUGIN}-{s}").is_file()]
        self.assertEqual(
            missing, [],
            f"copy_plugin_dir deleted deployed per-platform binaries for "
            f"suffixes {missing}, all of which _is_rebuild_artifact() in "
            f"scripts/check-plugin-rollout.py accepts as legitimately present "
            f"in a deployed dir with no crate counterpart.",
        )


class TestNonArtifactsAreStillDeleted(CopyPluginDirCase):
    """The control. Preserving rebuild artifacts must not become "delete
    nothing": a stale file that is NOT a rebuild artifact must still go, or the
    deployed dir stops being a mirror of the crate and this suite could be
    satisfied by simply dropping --delete."""

    def test_rsync_branch_deletes_stray_file(self):
        src, dst = self._make_src_crate(), self._make_deployed()
        self._run_copy(src, dst, use_rsync=True)
        self.assertFalse((dst / "stray-not-an-artifact.txt").exists(),
                         "a non-artifact stale file survived the mirror")

    def test_fallback_branch_deletes_stray_file(self):
        src, dst = self._make_src_crate(), self._make_deployed()
        self._run_copy(src, dst, use_rsync=False)
        self.assertFalse((dst / "stray-not-an-artifact.txt").exists(),
                         "a non-artifact stale file survived the mirror")

    def test_crate_files_are_mirrored(self):
        """And the copy still does its actual job."""
        src, dst = self._make_src_crate(), self._make_deployed()
        self._run_copy(src, dst, use_rsync=True)
        self.assertEqual(
            (dst / "bin" / PLUGIN).read_text(encoding="utf-8"),
            (src / "bin" / PLUGIN).read_text(encoding="utf-8"),
            "the launcher was not refreshed from the crate",
        )
        self.assertTrue((dst / ".claude-plugin" / "plugin.json").is_file())
        self.assertTrue((dst / "hooks" / "session-start.sh").is_file())


class TestOrphanedArtifactsAreDeleted(CopyPluginDirCase):
    """The protection is NARROW: `bin/<stem>-<suffix>` is spared the mirror's
    `--delete` only while the source crate still ships the launcher
    `bin/<stem>`.

    A stem with no launcher left in the crate is the residue of a binary that
    was renamed or removed. rebuild-plugins.sh builds what the crate declares,
    so nothing will ever write that file again; sparing it anyway makes the
    protection a one-way ratchet that only accumulates dead bytes in every
    deployed version dir, and makes `check-plugin-rollout.py`'s
    `_is_rebuild_artifact()` allowance a permanent hiding place for files no
    revision of the crate can account for.

    These cases are the half that the survive-the-recopy cases above cannot
    see: an implementation that protects `/bin/*-<suffix>` unconditionally
    satisfies every one of those and fails every one of these.
    """

    def _assert_orphan_deleted(self, dst, suffixes=("linux-x86_64",)):
        surviving = (sorted(p.name for p in (dst / "bin").iterdir())
                     if (dst / "bin").is_dir() else [])
        alive = [s for s in suffixes
                 if (dst / "bin" / f"{ORPHAN_STEM}-{s}").exists()]
        self.assertEqual(
            alive, [],
            f"copy_plugin_dir kept orphaned artifact(s) "
            f"bin/{ORPHAN_STEM}-<{','.join(alive)}> even though the crate ships "
            f"no bin/{ORPHAN_STEM} launcher for them. Surviving bin/ entries: "
            f"{surviving}. Nothing rebuilds those files, so the protection is "
            f"not preserving a live artifact -- it is refusing to ever delete "
            f"anything it once spared.",
        )

    def test_rsync_branch_deletes_orphaned_artifact(self):
        src = self._make_src_crate()
        # No live artifacts at all: the deployed dir's ONLY per-platform file is
        # the orphan, so the protect list must come out empty. That also walks
        # the empty-array path the narrowing introduced (`protect` used to be
        # unconditionally 8 entries long and can now be zero).
        dst = self._make_deployed(suffixes=())
        self._plant_orphan(dst)
        self._run_copy(src, dst, use_rsync=True)
        self._assert_orphan_deleted(dst)

    def test_fallback_branch_deletes_orphaned_artifact(self):
        src = self._make_src_crate()
        dst = self._make_deployed(suffixes=())
        self._plant_orphan(dst)
        self._run_copy(src, dst, use_rsync=False)
        self._assert_orphan_deleted(dst)

    def test_rsync_branch_deletes_orphans_for_every_platform_suffix(self):
        """Mirrors the every-suffix survival case: the narrowing has to hold for
        each suffix independently, including the `.exe` forms whose stem strip
        overlaps a shorter suffix in the same table."""
        src = self._make_src_crate()
        dst = self._make_deployed(suffixes=())
        self._plant_orphan(dst, suffixes=PLATFORM_SUFFIXES)
        self._run_copy(src, dst, use_rsync=True)
        self._assert_orphan_deleted(dst, suffixes=PLATFORM_SUFFIXES)

    def test_fallback_branch_deletes_orphans_for_every_platform_suffix(self):
        src = self._make_src_crate()
        dst = self._make_deployed(suffixes=())
        self._plant_orphan(dst, suffixes=PLATFORM_SUFFIXES)
        self._run_copy(src, dst, use_rsync=False)
        self._assert_orphan_deleted(dst, suffixes=PLATFORM_SUFFIXES)

    def test_rsync_branch_keeps_live_artifact_and_drops_orphan_together(self):
        """Both verdicts in ONE deployed dir, so neither can be reached by an
        all-or-nothing rule: protect everything and the orphan survives; protect
        nothing and the live artifact dies. Only a per-stem decision passes."""
        src = self._make_src_crate()
        dst = self._make_deployed(suffixes=("linux-x86_64",))
        self._plant_orphan(dst, suffixes=("linux-x86_64",))
        self._run_copy(src, dst, use_rsync=True)
        self._assert_orphan_deleted(dst)
        self.assertTrue(
            (dst / "bin" / f"{PLUGIN}-linux-x86_64").is_file(),
            f"deleting the orphan also took the live artifact "
            f"bin/{PLUGIN}-linux-x86_64 with it; the crate still ships "
            f"bin/{PLUGIN}, so that binary is the one thing the mirror must "
            f"not touch",
        )

    def test_fallback_branch_keeps_live_artifact_and_drops_orphan_together(self):
        src = self._make_src_crate()
        dst = self._make_deployed(suffixes=("linux-x86_64",))
        self._plant_orphan(dst, suffixes=("linux-x86_64",))
        self._run_copy(src, dst, use_rsync=False)
        self._assert_orphan_deleted(dst)
        self.assertTrue(
            (dst / "bin" / f"{PLUGIN}-linux-x86_64").is_file(),
            f"deleting the orphan also took the live artifact "
            f"bin/{PLUGIN}-linux-x86_64 with it; the crate still ships "
            f"bin/{PLUGIN}, so that binary is the one thing the mirror must "
            f"not touch",
        )


if __name__ == "__main__":
    unittest.main()
