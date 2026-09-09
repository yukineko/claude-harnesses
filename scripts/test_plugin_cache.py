#!/usr/bin/env python3
"""Tests for the shared cache facts: staleness and live-session holding.

These pin the rule BOTH consumers obey — the gate that reports superseded
version dirs and the pruner that deletes them. The dangerous direction here is
not a missed report, it is deleting a directory a running session is executing
from, so the "held" cases carry as much weight as the "removable" one.
"""
import contextlib
import importlib.util
import io
import os
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

_HERE = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location(
    "plugin_cache", os.path.join(_HERE, "plugin_cache.py")
)
pc = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(pc)

# The pruner is the other consumer of the same facts: scan() only says what is
# removable, and "removable" is only half the contract -- a dangling symlink
# that scan reports but shutil.rmtree cannot delete is still never removed. The
# filename is hyphenated, so it cannot be `import`ed by name.
_prune_spec = importlib.util.spec_from_file_location(
    "prune_plugin_cache", os.path.join(_HERE, "prune-plugin-cache.py")
)
prune = importlib.util.module_from_spec(_prune_spec)
_prune_spec.loader.exec_module(prune)


def _plugin(crates, name, version):
    pj = Path(crates) / name / ".claude-plugin" / "plugin.json"
    pj.parent.mkdir(parents=True, exist_ok=True)
    pj.write_text(f'{{"name": "{name}", "version": "{version}"}}', encoding="utf-8")


def _cached(cache, name, version, marker_pids=(), tmp_markers=()):
    d = Path(cache) / name / version
    d.mkdir(parents=True, exist_ok=True)
    (d / "payload.txt").write_text(version, encoding="utf-8")
    if marker_pids or tmp_markers:
        m = d / pc.IN_USE_DIR
        m.mkdir(exist_ok=True)
        for p in marker_pids:
            (m / str(p)).write_text("x", encoding="utf-8")
        for t in tmp_markers:
            (m / t).write_text("x", encoding="utf-8")
    return d


def _link(cache, name, version, target):
    """Put a symlink where a version dir is expected. `target` is not created
    here: whether it exists is exactly what each test varies."""
    d = Path(cache) / name
    d.mkdir(parents=True, exist_ok=True)
    link = d / version
    os.symlink(str(target), str(link))
    return link


def _plain_file(cache, name, version, body="not a version dir"):
    d = Path(cache) / name
    d.mkdir(parents=True, exist_ok=True)
    f = d / version
    f.write_text(body, encoding="utf-8")
    return f


class Liveness(unittest.TestCase):
    def test_own_pid_is_alive(self):
        self.assertIs(pc.pid_alive(os.getpid()), True)

    def test_pid_1_is_alive_even_though_not_ours(self):
        """EPERM means the process EXISTS. Reading it as dead would let the
        pruner delete a version dir held by another user's running session."""
        self.assertIs(pc.pid_alive(1), True)

    def test_absent_pid_is_dead(self):
        # Find a pid that is genuinely gone rather than assuming a magic number.
        dead = None
        for candidate in range(4194300, 4194200, -1):
            if pc.pid_alive(candidate) is False:
                dead = candidate
                break
        self.assertIsNotNone(dead, "no dead pid found to test with")
        self.assertIs(pc.pid_alive(dead), False)


class Staleness(unittest.TestCase):
    def _fixture(self, tmp):
        crates, cache = Path(tmp) / "crates", Path(tmp) / "cache"
        crates.mkdir()
        cache.mkdir()
        return crates, cache

    def test_superseded_dir_with_no_markers_is_removable(self):
        with tempfile.TemporaryDirectory() as tmp:
            crates, cache = self._fixture(tmp)
            _plugin(crates, "alpha", "2.0.0")
            _cached(cache, "alpha", "1.0.0")
            _cached(cache, "alpha", "2.0.0")
            cur, probs = pc.source_versions(str(crates))
            stale, sprobs = pc.scan(str(cache), cur)
            self.assertEqual(probs + sprobs, [])
            self.assertEqual([s.version for s in stale], ["1.0.0"])
            self.assertTrue(stale[0].removable)

    def test_current_version_is_never_stale(self):
        """Control arm: without this, a rule that called EVERY dir stale would
        satisfy the test above and hand the pruner the live directory."""
        with tempfile.TemporaryDirectory() as tmp:
            crates, cache = self._fixture(tmp)
            _plugin(crates, "alpha", "2.0.0")
            _cached(cache, "alpha", "2.0.0")
            cur, _ = pc.source_versions(str(crates))
            stale, _ = pc.scan(str(cache), cur)
            self.assertEqual(stale, [])

    def test_dir_held_by_live_pid_is_not_removable(self):
        with tempfile.TemporaryDirectory() as tmp:
            crates, cache = self._fixture(tmp)
            _plugin(crates, "alpha", "2.0.0")
            _cached(cache, "alpha", "1.0.0", marker_pids=[os.getpid()])
            cur, _ = pc.source_versions(str(crates))
            stale, _ = pc.scan(str(cache), cur)
            self.assertEqual(len(stale), 1)
            self.assertFalse(stale[0].removable)
            self.assertIn(os.getpid(), stale[0].holders.live_pids)

    def test_dead_markers_do_not_hold_a_dir(self):
        """The measured real-world state: 64 accumulated markers, all dead.
        If mere presence of .in_use held a dir, nothing would ever be pruned."""
        dead = next(
            c for c in range(4194300, 4194200, -1) if pc.pid_alive(c) is False
        )
        with tempfile.TemporaryDirectory() as tmp:
            crates, cache = self._fixture(tmp)
            _plugin(crates, "alpha", "2.0.0")
            _cached(cache, "alpha", "1.0.0", marker_pids=[dead],
                    tmp_markers=["19808.tmp.3e6c10da"])
            cur, _ = pc.source_versions(str(crates))
            stale, _ = pc.scan(str(cache), cur)
            self.assertTrue(stale[0].removable)

    def test_plugin_absent_from_source_yields_no_stale_dirs(self):
        """Not knowing which version is current must not mean 'all of them are
        stale' — that aims the pruner at the live dir."""
        with tempfile.TemporaryDirectory() as tmp:
            crates, cache = self._fixture(tmp)
            _cached(cache, "ghost", "1.0.0")
            _cached(cache, "ghost", "2.0.0")
            cur, _ = pc.source_versions(str(crates))
            stale, sprobs = pc.scan(str(cache), cur)
            self.assertEqual(stale, [])
            self.assertTrue(any("ghost" in p for p in sprobs))

    def test_unreadable_marker_dir_is_undetermined_and_kept(self):
        if os.geteuid() == 0:
            self.skipTest("root ignores directory permissions")
        with tempfile.TemporaryDirectory() as tmp:
            crates, cache = self._fixture(tmp)
            _plugin(crates, "alpha", "2.0.0")
            d = _cached(cache, "alpha", "1.0.0", marker_pids=[os.getpid()])
            marker = d / pc.IN_USE_DIR
            os.chmod(marker, 0o000)
            try:
                cur, _ = pc.source_versions(str(crates))
                stale, _ = pc.scan(str(cache), cur)
                self.assertEqual(len(stale), 1)
                self.assertIsNotNone(stale[0].holders.undetermined)
                self.assertFalse(
                    stale[0].removable,
                    "an uninspectable dir must be kept, not deleted",
                )
            finally:
                os.chmod(marker, 0o700)


class SettingsJsonPin(unittest.TestCase):
    """Reproduces the 2026-07-27 incident: a version dir with no `.in_use`
    marker at all, but referenced by an absolute path hardcoded into
    ~/.claude/settings.json, must not be removable. This is the actual bug —
    prune only looked at `.in_use`, so a settings.json-only pin was invisible
    and 8 hooks/statusLine broke when their pinned dirs got deleted."""

    def _fixture(self, tmp):
        crates, cache = Path(tmp) / "crates", Path(tmp) / "cache"
        crates.mkdir()
        cache.mkdir()
        return crates, cache

    def _settings(self, tmp, body):
        p = Path(tmp) / "settings.json"
        p.write_text(body, encoding="utf-8")
        return str(p)

    def test_dir_referenced_by_settings_json_hook_is_not_removable(self):
        with tempfile.TemporaryDirectory() as tmp:
            crates, cache = self._fixture(tmp)
            _plugin(crates, "ctxrot", "0.5.20")
            _cached(cache, "ctxrot", "0.5.18")  # no .in_use marker at all
            _cached(cache, "ctxrot", "0.5.20")
            settings = self._settings(
                tmp,
                '{"hooks": {"Stop": [{"hooks": [{"type": "command", '
                f'"command": "{cache}/ctxrot/0.5.18/bin/ctxrot-linux-x86_64 guard"}}]}}]}}}}',
            )
            pins, undetermined = pc.settings_pinned_versions(str(cache), paths=[settings])
            self.assertIsNone(undetermined)
            self.assertIn(("ctxrot", "0.5.18"), pins)

            cur, _ = pc.source_versions(str(crates))
            stale, _ = pc.scan(str(cache), cur, settings_pins=pins)
            self.assertEqual([s.version for s in stale], ["0.5.18"])
            self.assertFalse(
                stale[0].removable,
                "a dir pinned only by settings.json (no .in_use marker) must be kept",
            )
            self.assertTrue(stale[0].holders.pinned)

    def test_pin_for_one_plugin_does_not_protect_an_unrelated_stale_dir(self):
        """Control arm: a non-degenerate fix must still allow pruning of dirs
        that settings.json says nothing about."""
        with tempfile.TemporaryDirectory() as tmp:
            crates, cache = self._fixture(tmp)
            _plugin(crates, "ctxrot", "0.5.20")
            _plugin(crates, "beta", "2.0.0")
            _cached(cache, "ctxrot", "0.5.18")
            _cached(cache, "ctxrot", "0.5.20")
            _cached(cache, "beta", "1.0.0")
            _cached(cache, "beta", "2.0.0")
            settings = self._settings(
                tmp,
                '{"hooks": {"Stop": [{"hooks": [{"type": "command", '
                f'"command": "{cache}/ctxrot/0.5.18/bin/ctxrot-linux-x86_64 guard"}}]}}]}}}}',
            )
            pins, undetermined = pc.settings_pinned_versions(str(cache), paths=[settings])
            cur, _ = pc.source_versions(str(crates))
            stale, _ = pc.scan(str(cache), cur, settings_pins=pins)
            by_plugin = {s.plugin: s for s in stale}
            self.assertFalse(by_plugin["ctxrot"].removable)
            self.assertTrue(by_plugin["beta"].removable)

    def test_no_settings_json_pins_nothing(self):
        """Control arm: absence of a settings file must not itself protect
        anything (else every prune run would silently no-op)."""
        with tempfile.TemporaryDirectory() as tmp:
            crates, cache = self._fixture(tmp)
            _plugin(crates, "alpha", "2.0.0")
            _cached(cache, "alpha", "1.0.0")
            _cached(cache, "alpha", "2.0.0")
            missing = str(Path(tmp) / "does-not-exist.json")
            pins, undetermined = pc.settings_pinned_versions(str(cache), paths=[missing])
            self.assertEqual(pins, {})
            self.assertIsNone(undetermined)
            cur, _ = pc.source_versions(str(crates))
            stale, _ = pc.scan(str(cache), cur, settings_pins=pins)
            self.assertTrue(stale[0].removable)

    def test_unreadable_settings_json_is_undetermined_and_protects_everything(self):
        """settings.json existing but failing to parse must resolve to the
        restrictive side (kept), not be treated as 'no pins found'."""
        with tempfile.TemporaryDirectory() as tmp:
            crates, cache = self._fixture(tmp)
            _plugin(crates, "alpha", "2.0.0")
            _cached(cache, "alpha", "1.0.0")
            _cached(cache, "alpha", "2.0.0")
            settings = self._settings(tmp, "{not valid json")
            pins, undetermined = pc.settings_pinned_versions(str(cache), paths=[settings])
            self.assertIsNotNone(undetermined)

            cur, _ = pc.source_versions(str(crates))
            stale, _ = pc.scan(
                str(cache), cur, settings_pins=pins, settings_undetermined=undetermined
            )
            self.assertFalse(
                stale[0].removable,
                "an unparseable settings.json must keep dirs, not be read as zero pins",
            )
            self.assertIsNotNone(stale[0].holders.undetermined)


class NonVersionDirEntries(unittest.TestCase):
    """scan() used to `continue` on every entry that was not a directory, so a
    broken entry in the cache read as "nothing stale here" -- the fail-open
    CLAUDE.md 3. forbids, since "I could not account for this" was written out
    as "clean". Measured 2026-09-08: condukt/0.4.2 had been a symlink to a
    long-pruned 0.6.0 since 2026-07-02 and every prune run reported
    "0 stale dir(s)" with it sitting there.

    The two halves resolve to OPPOSITE actions and both matter: a dangling link
    addresses nothing, so removing it cannot lose anything; anything else is
    unaccounted state, and deletion is the irreversible action, so it is
    reported and left alone.
    """

    def _fixture(self, tmp):
        crates, cache = Path(tmp) / "crates", Path(tmp) / "cache"
        crates.mkdir()
        cache.mkdir()
        return crates, cache

    def test_dangling_symlink_is_stale_removable_and_names_its_target(self):
        """The measured condukt/0.4.2 -> 0.6.0 case. Silently skipping it is
        what made prune report a clean cache while it sat there."""
        with tempfile.TemporaryDirectory() as tmp:
            crates, cache = self._fixture(tmp)
            _plugin(crates, "condukt", "0.6.1")
            _cached(cache, "condukt", "0.6.1")
            gone = Path(cache) / "condukt" / "0.6.0"  # pruned long ago
            _link(cache, "condukt", "0.4.2", gone)

            cur, _ = pc.source_versions(str(crates))
            stale, sprobs = pc.scan(str(cache), cur)
            self.assertEqual(
                [(s.version, s.kind) for s in stale], [("0.4.2", "dangling-link")]
            )
            self.assertTrue(stale[0].removable)
            self.assertEqual(
                sprobs,
                [],
                "a dangling link is accounted for, so it is stale -- not an "
                "unresolved problem",
            )
            self.assertEqual(
                stale[0].describe(),
                f"condukt/0.4.2 (dangling symlink -> {gone})",
                "the log must say what the pointer pointed AT: 'pruned "
                "condukt/0.4.2' alone reads as though a version was reclaimed",
            )

    def test_dangling_symlink_does_not_mask_a_real_stale_dir_beside_it(self):
        """Control arm: a rule that returned the link INSTEAD of continuing to
        the rest of the plugin's entries would satisfy the test above."""
        with tempfile.TemporaryDirectory() as tmp:
            crates, cache = self._fixture(tmp)
            _plugin(crates, "condukt", "0.6.1")
            _cached(cache, "condukt", "0.6.1")
            _cached(cache, "condukt", "0.5.0")
            _link(cache, "condukt", "0.4.2", Path(tmp) / "gone")

            cur, _ = pc.source_versions(str(crates))
            stale, _ = pc.scan(str(cache), cur)
            self.assertEqual(
                sorted((s.version, s.kind) for s in stale),
                [("0.4.2", "dangling-link"), ("0.5.0", "dir")],
            )

    def test_dangling_symlink_is_kept_when_settings_json_is_undetermined(self):
        """"Could not check for a pin" must keep a dangling link for the same
        reason it keeps every real dir -- the link is exactly the shape a
        settings.json hook path would be pinned through."""
        with tempfile.TemporaryDirectory() as tmp:
            crates, cache = self._fixture(tmp)
            _plugin(crates, "condukt", "0.6.1")
            _cached(cache, "condukt", "0.6.1")
            _link(cache, "condukt", "0.4.2", Path(tmp) / "gone")

            settings = Path(tmp) / "settings.json"
            settings.write_text("{not valid json", encoding="utf-8")
            pins, undetermined = pc.settings_pinned_versions(
                str(cache), paths=[str(settings)]
            )
            self.assertIsNotNone(undetermined)

            cur, _ = pc.source_versions(str(crates))
            stale, _ = pc.scan(
                str(cache), cur, settings_pins=pins, settings_undetermined=undetermined
            )
            self.assertEqual(len(stale), 1)
            self.assertEqual(stale[0].kind, "dangling-link")
            self.assertFalse(
                stale[0].removable,
                "an unparseable settings.json must keep the link too, not be "
                "read as 'no pins found'",
            )
            self.assertIsNotNone(stale[0].holders.undetermined)

    def test_symlink_to_a_real_dir_keeps_the_ordinary_holder_rules(self):
        """os.path.isdir follows links, so a link to a LIVE directory is an
        ordinary version dir. Its `.in_use` holder must still protect it --
        otherwise the non-dir branch would have become a way to delete a
        running session's plugin through a pointer."""
        with tempfile.TemporaryDirectory() as tmp:
            crates, cache = self._fixture(tmp)
            _plugin(crates, "alpha", "2.0.0")
            _cached(cache, "alpha", "2.0.0")
            elsewhere = Path(tmp) / "elsewhere"
            held = _cached(elsewhere, "alpha", "1.0.0", marker_pids=[os.getpid()])
            _link(cache, "alpha", "1.0.0", held)

            cur, _ = pc.source_versions(str(crates))
            stale, sprobs = pc.scan(str(cache), cur)
            self.assertEqual(sprobs, [])
            self.assertEqual(len(stale), 1)
            self.assertEqual(
                stale[0].kind, "dir", "a link to a real dir is not a dangling link"
            )
            self.assertFalse(stale[0].removable)
            self.assertIn(os.getpid(), stale[0].holders.live_pids)

    def test_plain_file_in_the_cache_is_reported_and_left_on_disk(self):
        """Unaccounted state must be loud but must NOT be deleted on a guess:
        scan cannot say what this is, and "I do not know what this is" may not
        resolve to an irreversible delete."""
        with tempfile.TemporaryDirectory() as tmp:
            crates, cache = self._fixture(tmp)
            _plugin(crates, "alpha", "2.0.0")
            _cached(cache, "alpha", "2.0.0")
            stray = _plain_file(cache, "alpha", "1.0.0")

            cur, _ = pc.source_versions(str(crates))
            stale, sprobs = pc.scan(str(cache), cur)
            self.assertEqual(
                stale, [], "a plain file must never be handed to the remover"
            )
            self.assertEqual(len(sprobs), 1, sprobs)
            self.assertIn(str(stray), sprobs[0])
            self.assertIn("not a version dir", sprobs[0])
            self.assertTrue(stray.is_file(), "scan must not touch the filesystem")

    def test_symlink_to_an_existing_file_is_reported_and_left_on_disk(self):
        """The other half of the same rule, and the one that is easy to get
        wrong: os.path.islink() is TRUE for a link to a file, so classifying by
        islink alone calls a perfectly resolvable pointer "dangling" and marks
        it removable. Per scripts/prune-plugin-cache.py's own docstring --
        "anything else (a plain file, a link to a file) is REPORTED and left in
        place" -- this must land in problems, not in stale."""
        with tempfile.TemporaryDirectory() as tmp:
            crates, cache = self._fixture(tmp)
            _plugin(crates, "alpha", "2.0.0")
            _cached(cache, "alpha", "2.0.0")
            target = Path(tmp) / "some-real-file.txt"
            target.write_text("payload", encoding="utf-8")
            link = _link(cache, "alpha", "1.0.0", target)

            cur, _ = pc.source_versions(str(crates))
            stale, sprobs = pc.scan(str(cache), cur)
            self.assertEqual(
                [s.version for s in stale],
                [],
                "a link that RESOLVES is not dangling: unlinking it drops the "
                "only reference to a file that still exists",
            )
            self.assertEqual(len(sprobs), 1, sprobs)
            self.assertIn(str(link), sprobs[0])
            self.assertIn("not a version dir", sprobs[0])

    def test_dangling_symlink_on_the_current_version_is_reported_not_stale(self):
        """The worst case of all: the LIVE version is a broken pointer, so the
        plugin is not on disk at all. It is not stale (the current version is
        never pruned), so if it were only checked after the `v == cur` skip it
        would produce `0 stale, 0 problems` -- a clean report about a plugin
        that cannot run. Being unremovable is not the same as being fine."""
        with tempfile.TemporaryDirectory() as tmp:
            crates, cache = self._fixture(tmp)
            _plugin(crates, "condukt", "0.6.1")
            link = _link(cache, "condukt", "0.6.1", Path(tmp) / "never-rolled-out")

            cur, _ = pc.source_versions(str(crates))
            stale, sprobs = pc.scan(str(cache), cur)
            self.assertEqual(
                stale,
                [],
                "the current version must never be handed to the remover, "
                "broken pointer or not",
            )
            self.assertEqual(len(sprobs), 1, sprobs)
            self.assertIn(str(link), sprobs[0])
            self.assertIn(
                "dangling symlink",
                sprobs[0],
                "it must not be lumped in with the generic 'not a version dir' "
                "message -- the live plugin being absent is a different fact",
            )
            self.assertTrue(
                os.path.lexists(str(link)), "scan must not touch the filesystem"
            )

    def test_current_dangling_link_does_not_shield_a_stale_link_beside_it(self):
        """Control arm for the rule above: handling the current-version case by
        bailing out of the plugin (or by reporting every non-dir entry as a
        problem) would make the whole plugin dir unprunable. The two entries
        differ only in whether they are the current version, and they must get
        opposite answers."""
        with tempfile.TemporaryDirectory() as tmp:
            crates, cache = self._fixture(tmp)
            _plugin(crates, "condukt", "0.6.1")
            _link(cache, "condukt", "0.6.1", Path(tmp) / "never-rolled-out")
            _link(cache, "condukt", "0.4.2", Path(tmp) / "pruned-long-ago")

            cur, sprobs_src = pc.source_versions(str(crates))
            stale, sprobs = pc.scan(str(cache), cur)
            self.assertEqual(sprobs_src, [])
            self.assertEqual(
                [(s.version, s.kind, s.removable) for s in stale],
                [("0.4.2", "dangling-link", True)],
                "the non-current broken pointer is still removable",
            )
            self.assertEqual(len(sprobs), 1, sprobs)
            self.assertIn("0.6.1", sprobs[0])

    def test_kept_dangling_link_names_both_what_it_is_and_why_it_was_kept(self):
        """describe() carries two independent facts: WHAT the entry is and WHY
        the pruner left it alone. Rendering the kind as an early return drops
        the second one, so a link kept because settings.json would not parse
        printed as though it had simply been skipped for being broken."""
        with tempfile.TemporaryDirectory() as tmp:
            crates, cache = self._fixture(tmp)
            _plugin(crates, "condukt", "0.6.1")
            _cached(cache, "condukt", "0.6.1")
            gone = Path(tmp) / "pruned-long-ago"
            _link(cache, "condukt", "0.4.2", gone)

            settings = Path(tmp) / "settings.json"
            settings.write_text("{not valid json", encoding="utf-8")
            pins, undetermined = pc.settings_pinned_versions(
                str(cache), paths=[str(settings)]
            )
            cur, _ = pc.source_versions(str(crates))
            stale, _ = pc.scan(
                str(cache), cur, settings_pins=pins, settings_undetermined=undetermined
            )
            self.assertEqual(len(stale), 1)
            desc = stale[0].describe()
            self.assertIn(f"(dangling symlink -> {gone})", desc, desc)
            self.assertIn("(undetermined: ", desc, desc)
            self.assertLess(
                desc.index("dangling symlink"),
                desc.index("undetermined"),
                "kind is a prefix on the identity, the hold reason comes after",
            )

    def test_plain_dangling_link_still_renders_without_a_hold_reason(self):
        """Control arm for the rendering above: appending the hold reason must
        not start appending an empty one to every unheld entry."""
        with tempfile.TemporaryDirectory() as tmp:
            crates, cache = self._fixture(tmp)
            _plugin(crates, "condukt", "0.6.1")
            _cached(cache, "condukt", "0.6.1")
            gone = Path(tmp) / "pruned-long-ago"
            _link(cache, "condukt", "0.4.2", gone)

            cur, _ = pc.source_versions(str(crates))
            stale, _ = pc.scan(str(cache), cur)
            self.assertEqual(
                stale[0].describe(), f"condukt/0.4.2 (dangling symlink -> {gone})"
            )


class PrunerRemoval(unittest.TestCase):
    """scan() saying "removable" is not the same as the pruner being able to
    remove it. shutil.rmtree raises NotADirectoryError on a symlink, so a
    dangling link reported as stale but removed with rmtree would be logged as
    a failure on every single run and never actually go away."""

    def _fixture(self, tmp):
        crates, cache = Path(tmp) / "crates", Path(tmp) / "cache"
        crates.mkdir()
        cache.mkdir()
        return crates, cache

    def _run_prune(self, repo, cache, settings=None, extra=()):
        """Run the pruner in-process with a settings.json of OUR choosing.

        Without the env override the pruner reads the real ~/.claude/settings.json;
        a test must never depend on (or be steered by) the user's live one.
        """
        out, err = io.StringIO(), io.StringIO()
        settings = settings or str(Path(repo) / "no-such-settings.json")
        env = {"CLAUDE_SETTINGS_JSON": settings}
        with mock.patch.dict(os.environ, env):
            with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
                rc = prune.main(
                    ["--repo", str(repo), "--cache", str(cache), *extra]
                )
        return out.getvalue(), err.getvalue(), rc

    def test_pruner_unlinks_a_dangling_symlink_and_logs_its_target(self):
        with tempfile.TemporaryDirectory() as tmp:
            crates, cache = self._fixture(tmp)
            _plugin(crates, "condukt", "0.6.1")
            _cached(cache, "condukt", "0.6.1")
            gone = Path(cache) / "condukt" / "0.6.0"
            link = _link(cache, "condukt", "0.4.2", gone)

            out, err, rc = self._run_prune(tmp, cache)

            self.assertFalse(
                os.path.lexists(str(link)),
                "the broken pointer must actually be gone -- rmtree would have "
                f"raised NotADirectoryError and left it. stdout={out!r} "
                f"stderr={err!r}",
            )
            self.assertIn(f"pruned condukt/0.4.2 (dangling symlink -> {gone})", out)
            self.assertNotIn(
                "-> ?",
                out,
                "describe() must be rendered BEFORE the unlink, or the log "
                "stops saying what was removed",
            )
            self.assertNotIn("NotADirectoryError", out + err)
            self.assertEqual(rc, 0, f"stdout={out!r} stderr={err!r}")
            self.assertTrue(
                (Path(cache) / "condukt" / "0.6.1").is_dir(),
                "the current version dir must be untouched",
            )

    def test_pruner_leaves_an_unaccounted_file_and_exits_nonzero(self):
        """Reported, kept, and LOUD: exit 0 with the entry still sitting there
        is the "0 stale dir(s)" report that hid condukt/0.4.2 for two months."""
        with tempfile.TemporaryDirectory() as tmp:
            crates, cache = self._fixture(tmp)
            _plugin(crates, "alpha", "2.0.0")
            _cached(cache, "alpha", "2.0.0")
            stray = _plain_file(cache, "alpha", "1.0.0")

            out, err, rc = self._run_prune(tmp, cache)

            self.assertTrue(stray.is_file(), "an unaccounted file must be kept")
            self.assertIn("PROBLEM", err)
            self.assertIn("not a version dir", err)
            self.assertEqual(
                rc, 1, f"an unaccounted entry must not exit 0. stdout={out!r}"
            )

    def test_pruner_removes_only_the_dangling_link_and_keeps_every_holder(self):
        """Non-degeneracy arm for the whole change: in one cache holding a
        dangling link, a live-pid holder, a settings.json pin and the current
        version, exactly one thing may disappear."""
        with tempfile.TemporaryDirectory() as tmp:
            crates, cache = self._fixture(tmp)
            _plugin(crates, "alpha", "2.0.0")
            cur_dir = _cached(cache, "alpha", "2.0.0")
            held = _cached(cache, "alpha", "1.9.0", marker_pids=[os.getpid()])
            pinned = _cached(cache, "alpha", "1.8.0")
            link = _link(cache, "alpha", "1.7.0", Path(tmp) / "gone")

            settings = Path(tmp) / "settings.json"
            settings.write_text(
                '{"hooks": {"Stop": [{"hooks": [{"type": "command", "command": '
                f'"{cache}/alpha/1.8.0/bin/alpha-linux-x86_64 guard"}}]}}]}}}}',
                encoding="utf-8",
            )

            out, err, rc = self._run_prune(tmp, cache, settings=str(settings))

            self.assertFalse(os.path.lexists(str(link)), f"stdout={out!r}")
            for keep in (cur_dir, held, pinned):
                self.assertTrue(keep.is_dir(), f"{keep} must be kept. stdout={out!r}")
            self.assertIn("kept alpha/1.9.0 (in use by pid", out)
            self.assertIn("kept alpha/1.8.0 (pinned by settings.json", out)
            self.assertEqual(rc, 0, f"stdout={out!r} stderr={err!r}")

    def test_pruner_keeps_a_broken_current_version_and_exits_nonzero(self):
        """End to end for the current-version case: the broken pointer to the
        LIVE version survives the run (nothing may delete the current version),
        its stale sibling is still reclaimed in that same run, and the run does
        NOT report success -- a plugin that is not on disk must not be logged as
        a clean prune."""
        with tempfile.TemporaryDirectory() as tmp:
            crates, cache = self._fixture(tmp)
            _plugin(crates, "condukt", "0.6.1")
            current = _link(cache, "condukt", "0.6.1", Path(tmp) / "never-rolled-out")
            sibling = _link(cache, "condukt", "0.4.2", Path(tmp) / "pruned-long-ago")

            out, err, rc = self._run_prune(tmp, cache)

            self.assertTrue(
                os.path.lexists(str(current)),
                f"the current version must survive the prune. stdout={out!r}",
            )
            self.assertFalse(
                os.path.lexists(str(sibling)),
                "the non-current broken pointer must still be reclaimed in the "
                f"same run. stdout={out!r}",
            )
            self.assertIn("PROBLEM", err)
            self.assertIn("dangling symlink", err)
            self.assertIn(str(current), err)
            self.assertEqual(
                rc, 1, f"a missing current version must not exit 0. stdout={out!r}"
            )

    def test_pruner_kept_line_names_both_the_broken_pointer_and_the_reason(self):
        """The `kept` line is where describe() is actually read. "kept
        condukt/0.4.2 (dangling symlink -> x)" alone reads as though the pruner
        declined because the link was broken; the reason it declined is that
        settings.json could not be parsed, and both belong in the line."""
        with tempfile.TemporaryDirectory() as tmp:
            crates, cache = self._fixture(tmp)
            _plugin(crates, "condukt", "0.6.1")
            _cached(cache, "condukt", "0.6.1")
            gone = Path(tmp) / "pruned-long-ago"
            link = _link(cache, "condukt", "0.4.2", gone)

            settings = Path(tmp) / "settings.json"
            settings.write_text("{not valid json", encoding="utf-8")

            out, err, rc = self._run_prune(tmp, cache, settings=str(settings))

            self.assertTrue(
                os.path.lexists(str(link)),
                f"an undetermined settings.json must keep the link. stdout={out!r}",
            )
            kept = [ln for ln in out.splitlines() if ln.startswith("kept ")]
            self.assertEqual(len(kept), 1, out)
            self.assertIn(f"(dangling symlink -> {gone})", kept[0])
            self.assertIn("(undetermined: ", kept[0])
            self.assertEqual(rc, 1, f"stdout={out!r} stderr={err!r}")



def _run_prune_main(repo, cache, settings=None, extra=()):
    """Module-level twin of PrunerRemoval._run_prune, so the classes below can
    drive the real pruner end to end without reaching into another TestCase.

    The CLAUDE_SETTINGS_JSON override is not optional: without it the pruner
    reads the user's live ~/.claude/settings.json and a test's verdict starts
    depending on whose machine it runs on.
    """
    out, err = io.StringIO(), io.StringIO()
    settings = settings or str(Path(repo) / "no-such-settings.json")
    with mock.patch.dict(os.environ, {"CLAUDE_SETTINGS_JSON": settings}):
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            rc = prune.main(["--repo", str(repo), "--cache", str(cache), *extra])
    return out.getvalue(), err.getvalue(), rc


class PluginLevelNonDirEntries(unittest.TestCase):
    """The same silent skip as NonVersionDirEntries, one level UP.

    scan() looped `for pname in ...: if not os.path.isdir(pdir): continue`, so a
    plain file or a broken pointer sitting at `<cache>/<plugin>` produced
    `stale == [] and problems == []` -- the cache reported CLEAN about an entry
    the code could not account for at all. CLAUDE.md 3.: an input that is
    neither returned in `stale` nor reported in `problems` is "cannot determine"
    written out as "fine".

    Nothing at this level is prunable -- there is no version to reason about, so
    no StaleDir can even describe it -- which is precisely why the only correct
    output is a problem. "Unremovable" is not "unremarkable".
    """

    def _fixture(self, tmp):
        crates, cache = Path(tmp) / "crates", Path(tmp) / "cache"
        crates.mkdir()
        cache.mkdir()
        return crates, cache

    def test_plain_file_at_the_plugin_level_is_reported_and_never_stale(self):
        with tempfile.TemporaryDirectory() as tmp:
            crates, cache = self._fixture(tmp)
            _plugin(crates, "alpha", "2.0.0")
            stray = Path(cache) / "README"
            stray.write_text("not a plugin dir", encoding="utf-8")

            cur, _ = pc.source_versions(str(crates))
            stale, sprobs = pc.scan(str(cache), cur)

            self.assertEqual(
                stale, [], "nothing at the plugin level may be handed to the remover"
            )
            self.assertEqual(
                len(sprobs),
                1,
                f"a plugin-level stray must be reported, not silently skipped: {sprobs}",
            )
            self.assertIn(str(stray), sprobs[0])
            self.assertTrue(stray.is_file(), "scan must not touch the filesystem")

    def test_dangling_symlink_at_the_plugin_level_is_reported_and_never_stale(self):
        """A broken pointer at `<cache>/<plugin>` is the exact shape of the
        condukt/0.4.2 incident, moved one directory up. It must not be pruned
        either: a StaleDir needs a (plugin, version) pair and this entry has no
        version, so the pruner has nothing it could correctly log removing."""
        with tempfile.TemporaryDirectory() as tmp:
            crates, cache = self._fixture(tmp)
            _plugin(crates, "alpha", "2.0.0")
            gone = Path(tmp) / "pruned-long-ago"
            link = Path(cache) / "condukt"
            os.symlink(str(gone), str(link))

            cur, _ = pc.source_versions(str(crates))
            stale, sprobs = pc.scan(str(cache), cur)

            self.assertEqual(stale, [], "a plugin-level pointer has no version to prune")
            self.assertEqual(
                len(sprobs),
                1,
                f"a plugin-level broken pointer must be reported: {sprobs}",
            )
            self.assertIn(str(link), sprobs[0])
            self.assertTrue(
                os.path.lexists(str(link)), "scan must not touch the filesystem"
            )

    def test_plugin_level_stray_does_not_hide_a_real_plugin_beside_it(self):
        """Control arm: reporting the stray by bailing out of the whole scan
        (or by returning early) would satisfy both tests above while making the
        rest of the cache unprunable. Exactly one problem, and the real
        plugin's superseded dir is still reclaimed."""
        with tempfile.TemporaryDirectory() as tmp:
            crates, cache = self._fixture(tmp)
            _plugin(crates, "alpha", "2.0.0")
            _cached(cache, "alpha", "1.0.0")
            _cached(cache, "alpha", "2.0.0")
            (Path(cache) / "README").write_text("stray", encoding="utf-8")
            os.symlink(str(Path(tmp) / "gone"), str(Path(cache) / "zzz-broken"))

            cur, _ = pc.source_versions(str(crates))
            stale, sprobs = pc.scan(str(cache), cur)

            self.assertEqual([(s.version, s.removable) for s in stale], [("1.0.0", True)])
            self.assertEqual(len(sprobs), 2, sprobs)

    def test_pruner_leaves_a_plugin_level_stray_and_exits_nonzero(self):
        """End to end: the entry survives the run AND the run is loud. Exit 0
        with it still sitting there is the "0 stale dir(s)" report that hid the
        measured breakage for two months."""
        with tempfile.TemporaryDirectory() as tmp:
            crates, cache = self._fixture(tmp)
            _plugin(crates, "alpha", "2.0.0")
            _cached(cache, "alpha", "2.0.0")
            stray = Path(cache) / "README"
            stray.write_text("stray", encoding="utf-8")
            link = Path(cache) / "condukt"
            os.symlink(str(Path(tmp) / "gone"), str(link))

            out, err, rc = _run_prune_main(tmp, cache)

            self.assertTrue(stray.is_file(), f"stdout={out!r}")
            self.assertTrue(os.path.lexists(str(link)), f"stdout={out!r}")
            self.assertNotIn("pruned ", out)
            self.assertIn("PROBLEM", err)
            self.assertIn(str(stray), err)
            self.assertIn(str(link), err)
            self.assertEqual(
                rc, 1, f"an unaccounted plugin-level entry must not exit 0. stdout={out!r}"
            )


class UninspectableVersionEntry(unittest.TestCase):
    """EACCES is not ENOENT.

    The version-level classifier asked `os.path.islink(vdir) and not
    os.path.exists(vdir)`. `os.path.exists()` swallows the errno and answers
    False for BOTH "the target is really gone" (ENOENT) and "a component of the
    target's path is not traversable" (EACCES). So a symlink pointing at a LIVE
    directory behind a chmod-000 parent was classified `kind="dangling-link"`,
    returned as removable `stale`, and the pruner UNLINKED it with exit 0 while
    printing "pruned <p>/<v> (dangling symlink -> ...)" -- the only pointer to
    live data deleted because the check was not allowed to look at it.

    That is the worst form of the fail-open CLAUDE.md 3. forbids: not merely
    reporting clean about something uninspected, but taking the irreversible
    action on it.
    """

    def _fixture(self, tmp):
        crates, cache = Path(tmp) / "crates", Path(tmp) / "cache"
        crates.mkdir()
        cache.mkdir()
        return crates, cache

    def _link_behind_locked_parent(self, tmp, cache, plugin, version):
        """A symlink at <cache>/<plugin>/<version> pointing at a real, populated
        directory whose PARENT is unreadable. Returns (link, unlock)."""
        outer = Path(tmp) / "locked"
        target = outer / "payload-dir"
        target.mkdir(parents=True)
        (target / "payload.txt").write_text("live bytes", encoding="utf-8")
        link = _link(cache, plugin, version, target)
        os.chmod(outer, 0o000)
        return link, (lambda: os.chmod(outer, 0o755))

    def test_symlink_behind_an_unreadable_parent_is_a_problem_not_stale(self):
        if os.geteuid() == 0:
            self.skipTest("root ignores directory permissions, so EACCES cannot occur")
        with tempfile.TemporaryDirectory() as tmp:
            crates, cache = self._fixture(tmp)
            _plugin(crates, "alpha", "2.0.0")
            _cached(cache, "alpha", "2.0.0")
            link, unlock = self._link_behind_locked_parent(tmp, cache, "alpha", "1.0.0")
            try:
                # The premise, asserted rather than assumed: this is exactly the
                # input on which exists() lies.
                self.assertTrue(os.path.islink(str(link)))
                self.assertFalse(
                    os.path.exists(str(link)),
                    "premise: exists() answers False here even though the target "
                    "is alive -- that is the whole bug",
                )

                cur, _ = pc.source_versions(str(crates))
                stale, sprobs = pc.scan(str(cache), cur)

                self.assertEqual(
                    stale,
                    [],
                    "a pointer that could not be resolved must never be handed "
                    "to the remover: unlinking it may drop the only reference "
                    "to live data",
                )
                self.assertEqual(len(sprobs), 1, sprobs)
                self.assertIn(str(link), sprobs[0])
                self.assertIn(
                    "EACCES",
                    sprobs[0],
                    "the report must name the errno, or 'cannot tell' is "
                    "indistinguishable from 'gone'",
                )
                self.assertNotIn(
                    "dangling symlink ->",
                    sprobs[0],
                    "an uninspectable entry must not be ASSERTED to be a "
                    "dangling pointer -- that phrase is the classification, "
                    "and it is the one the pruner acts on",
                )
                self.assertTrue(
                    os.path.lexists(str(link)), "scan must not touch the filesystem"
                )
            finally:
                unlock()

    def test_the_same_link_is_an_ordinary_version_dir_once_its_parent_opens(self):
        """Control arm: the permission bit is the ONLY difference between this
        test and the one above. A rule that called every symlink undetermined
        would satisfy that one and silence the real dangling-link case, so this
        pins that the very same link resolves to an ordinary, prunable version
        dir the moment it can actually be inspected."""
        if os.geteuid() == 0:
            self.skipTest("root ignores directory permissions, so EACCES cannot occur")
        with tempfile.TemporaryDirectory() as tmp:
            crates, cache = self._fixture(tmp)
            _plugin(crates, "alpha", "2.0.0")
            _cached(cache, "alpha", "2.0.0")
            _link, unlock = self._link_behind_locked_parent(tmp, cache, "alpha", "1.0.0")
            unlock()

            cur, _ = pc.source_versions(str(crates))
            stale, sprobs = pc.scan(str(cache), cur)

            self.assertEqual(sprobs, [], sprobs)
            self.assertEqual([(s.version, s.kind) for s in stale], [("1.0.0", "dir")])
            self.assertTrue(stale[0].removable)

    def test_a_symlink_cycle_is_still_dangling_and_removable(self):
        """Non-degeneracy for the errno split: it must not collapse to "any
        OSError is undetermined". A cycle resolves to nothing by construction,
        so there is no payload behind it to lose -- same reason ENOENT is
        removable."""
        with tempfile.TemporaryDirectory() as tmp:
            crates, cache = self._fixture(tmp)
            _plugin(crates, "alpha", "2.0.0")
            _cached(cache, "alpha", "2.0.0")
            pdir = Path(cache) / "alpha"
            os.symlink(str(pdir / "1.0.0"), str(pdir / "0.9.0"))
            os.symlink(str(pdir / "0.9.0"), str(pdir / "1.0.0"))

            cur, sprobs_src = pc.source_versions(str(crates))
            stale, sprobs = pc.scan(str(cache), cur)

            self.assertEqual(sprobs_src, [])
            self.assertEqual(sprobs, [], sprobs)
            self.assertEqual(
                sorted((s.version, s.kind, s.removable) for s in stale),
                [("0.9.0", "dangling-link", True), ("1.0.0", "dangling-link", True)],
            )

    def test_pruner_does_not_unlink_an_uninspectable_symlink(self):
        """End to end, and the assertion that actually matters: the measured
        pre-fix run printed "pruned alpha/1.0.0 (dangling symlink -> ...)" and
        exited 0 with the pointer gone. The link must survive, no "pruned" line
        may name it, and the run must not report success."""
        if os.geteuid() == 0:
            self.skipTest("root ignores directory permissions, so EACCES cannot occur")
        with tempfile.TemporaryDirectory() as tmp:
            crates, cache = self._fixture(tmp)
            _plugin(crates, "alpha", "2.0.0")
            _cached(cache, "alpha", "2.0.0")
            link, unlock = self._link_behind_locked_parent(tmp, cache, "alpha", "1.0.0")
            try:
                out, err, rc = _run_prune_main(tmp, cache)

                self.assertTrue(
                    os.path.lexists(str(link)),
                    "the pointer to live data must still be there. "
                    f"stdout={out!r} stderr={err!r}",
                )
                self.assertNotIn("pruned ", out)
                self.assertNotIn("dangling symlink", out)
                self.assertIn("PROBLEM", err)
                self.assertIn("EACCES", err)
                self.assertEqual(
                    rc,
                    1,
                    f"an uninspectable cache entry must not exit 0. stdout={out!r}",
                )
            finally:
                unlock()


if __name__ == "__main__":
    unittest.main()
