#!/usr/bin/env python3
"""Session-age hold for prune-plugin-cache.py (backlog 18fe626f).

Defect: a running Claude Code session pins its plugin dirs at session start, but
prune holds a superseded version dir only through `.in_use` markers, settings
pins and the registry, and Claude Code writes `.in_use` for only some plugins.
So prune deletes a dir a live session is still executing from.

Contract (user ruling 2026-10-03):
  * A superseded version dir is KEPT while any live `claude` process that
    STARTED BEFORE that dir was superseded is alive. "Superseded at" is when the
    newer, current version dir came into existence.
  * It is pruned only when it is superseded AND every live `claude` process
    started after it was superseded (or there is none). Other holders still apply.
  * If the process list or start times cannot be read, prune keeps everything
    and exits non-zero (fail closed).

SEAMS the implementer must match (proposals made by the test author)
====================================================================
Both are read from os.environ at CALL time (not import time) by
scripts/prune-plugin-cache.py. The tests run the script as a subprocess
(`python3 scripts/prune-plugin-cache.py --repo R --cache C`) with HOME,
CLAUDE_PLUGIN_REGISTRY and CLAUDE_SETTINGS_JSON pointed at temp paths.

1. PLUGIN_CACHE_PROC_LIST_PROBE
   A shell command (run with shell=True) that prints one live process per line:

       <pid> <start_epoch> <comm>

   whitespace-separated; pid = decimal int, start_epoch = decimal integer UNIX
   seconds, comm = the rest of the line, a bare name such as `claude`/`zsh`.
   (Tests only ever use bare names; matching a path by basename is the
   implementer's choice and is not tested.)
   Decisions pinned here:
     - non-zero exit                         -> undetermined
     - any non-blank line that does not parse as above -> undetermined
     - exit 0 but ZERO parsed lines          -> undetermined. A successful probe
       can never legitimately list zero processes because prune itself (python)
       is running, so "empty" cannot mean "no sessions"; it means the probe
       told us nothing. Treating it as "no claude, prune freely" would be the
       fail-open CLAUDE.md 3 forbids ("empty set is not clean").
   Undetermined => prune removes NOTHING and exits non-zero.
   When the variable is unset the implementation must fall back to a real
   probe (e.g. `ps`); tests never rely on that, and never on real processes.

2. PLUGIN_CACHE_SUPERSEDED_AT_OVERRIDE
   A JSON object mapping "<plugin>/<version>" -> superseded-at UNIX epoch
   (integer seconds) for each superseded dir, e.g.
       {"backlog/0.3.21": 1000000}
   No existing mechanism in plugin_cache.py/prune fits (nothing records when a
   dir was superseded; the nearest derivable fact is the birth/mtime of the
   newer sibling dir, which a test cannot set deterministically), so this is a
   new seam. In production the implementer derives/records the value however it
   likes; the override only replaces that derivation for the named dirs.
   "Started before" is compared as start_epoch < superseded_at (strict).
   v2 (18fe626f, exact per-version hold): production now derives the exact
   interval activated_at <= start < superseded_at from the rollout's
   version-history ledger (see test_prune_version_history.py). This seam
   keeps the meaning "superseded_at known, activated_at -inf", which is
   exactly the contract the cases below assert.

Fixture: backlog/0.3.21 (old) and backlog/0.3.22 (current in crates/ and in the
registry), no `.in_use`, no settings pin.
"""
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

_HERE = os.path.dirname(os.path.abspath(__file__))
_PRUNE = os.path.join(_HERE, "prune-plugin-cache.py")

SUPERSEDED_AT = 1_000_000  # when 0.3.21 was superseded by 0.3.22
BEFORE = SUPERSEDED_AT - 1_000  # a session started before the supersession
AFTER = SUPERSEDED_AT + 1_000  # a session started after it
# A process line that is always present in a well-formed probe answer: prune
# itself is alive, so a real probe can never be empty.
SELF_LINE = "424200 1 python3"


def _mk_version(cache, plugin, version):
    d = Path(cache) / plugin / version
    d.mkdir(parents=True, exist_ok=True)
    (d / "payload.txt").write_text(version, encoding="utf-8")
    return d


class SessionAgeHold(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.tmp = Path(self._tmp.name)
        self.crates = self.tmp / "crates"
        self.cache = self.tmp / "cache"
        self.crates.mkdir()
        self.cache.mkdir()
        pj = self.crates / "backlog" / ".claude-plugin" / "plugin.json"
        pj.parent.mkdir(parents=True)
        pj.write_text('{"name": "backlog", "version": "0.3.22"}', encoding="utf-8")
        self.cur = _mk_version(self.cache, "backlog", "0.3.22")
        self.registry = self.tmp / "installed_plugins.json"
        self.registry.write_text(
            json.dumps(
                {"plugins": {"backlog@yukineko": [{"installPath": str(self.cur)}]}}
            ),
            encoding="utf-8",
        )
        self.settings = self.tmp / "settings.json"
        self.settings.write_text("{}", encoding="utf-8")

    # -- helpers ---------------------------------------------------------
    def _run(self, probe, superseded_at):
        """Run the real pruner as a subprocess. `probe` is the shell command
        for PLUGIN_CACHE_PROC_LIST_PROBE; never reads real processes."""
        env = {
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
            "HOME": str(self.tmp / "home"),
            "CLAUDE_PLUGIN_CACHE": str(self.cache),
            "CLAUDE_PLUGIN_REGISTRY": str(self.registry),
            "CLAUDE_SETTINGS_JSON": str(self.settings),
            "PLUGIN_CACHE_PROC_LIST_PROBE": probe,
            "PLUGIN_CACHE_SUPERSEDED_AT_OVERRIDE": json.dumps(superseded_at),
        }
        p = subprocess.run(
            [
                sys.executable,
                _PRUNE,
                "--repo",
                str(self.tmp),
                "--cache",
                str(self.cache),
                "--registry",
                str(self.registry),
            ],
            env=env,
            capture_output=True,
            text=True,
            timeout=60,
        )
        return p.stdout, p.stderr, p.returncode

    def _probe_printing(self, *lines):
        f = self.tmp / "probe-output.txt"
        f.write_text("".join(ln + "\n" for ln in lines), encoding="utf-8")
        return f"cat '{f}'"

    def _old(self, version="0.3.21"):
        return _mk_version(self.cache, "backlog", version)

    # -- 1. RED today ----------------------------------------------------
    def test_1_claude_started_before_supersession_holds_the_old_dir(self):
        old = self._old()
        probe = self._probe_printing(SELF_LINE, f"9001 {BEFORE} claude")
        out, err, rc = self._run(probe, {"backlog/0.3.21": SUPERSEDED_AT})
        self.assertTrue(
            old.is_dir(),
            "a live claude that started before 0.3.21 was superseded still "
            f"uses it; it must be kept. rc={rc} stdout={out!r} stderr={err!r}",
        )
        self.assertTrue(self.cur.is_dir())

    # -- 2. GUARD --------------------------------------------------------
    def test_2_only_claude_started_after_supersession_does_not_hold(self):
        old = self._old()
        probe = self._probe_printing(SELF_LINE, f"9001 {AFTER} claude")
        out, err, rc = self._run(probe, {"backlog/0.3.21": SUPERSEDED_AT})
        self.assertFalse(
            old.exists(),
            f"every live claude started after the supersession; prune it. "
            f"rc={rc} stdout={out!r} stderr={err!r}",
        )
        self.assertIn("pruned backlog/0.3.21", out)
        self.assertEqual(rc, 0, f"stdout={out!r} stderr={err!r}")
        self.assertTrue(self.cur.is_dir(), "the current version is never pruned")

    # -- 3. GUARD --------------------------------------------------------
    def test_3_non_claude_process_started_before_does_not_hold(self):
        old = self._old()
        probe = self._probe_printing(
            SELF_LINE, f"9001 {BEFORE} zsh", f"9002 {BEFORE} node"
        )
        out, err, rc = self._run(probe, {"backlog/0.3.21": SUPERSEDED_AT})
        self.assertFalse(
            old.exists(),
            f"only `claude` processes hold; zsh/node must not. "
            f"rc={rc} stdout={out!r} stderr={err!r}",
        )
        self.assertIn("pruned backlog/0.3.21", out)
        self.assertEqual(rc, 0, f"stdout={out!r} stderr={err!r}")

    # -- 4. RED today ----------------------------------------------------
    def test_4_probe_nonzero_exit_keeps_everything_and_fails(self):
        old = self._old()
        # The probe prints a perfectly parseable "nobody holds" answer BEFORE
        # failing, so honouring stdout while ignoring the exit status would
        # prune; only a probe that checks the status keeps the dir.
        probe = f"echo '{SELF_LINE}'; echo '9001 {AFTER} claude'; exit 7"
        out, err, rc = self._run(probe, {"backlog/0.3.21": SUPERSEDED_AT})
        self.assertTrue(
            old.is_dir(),
            f"undetermined process list must keep the dir. stdout={out!r} stderr={err!r}",
        )
        self.assertNotEqual(rc, 0, f"fail closed means non-zero. stdout={out!r}")

    # -- 5. RED today ----------------------------------------------------
    def test_5_empty_or_garbage_probe_output_keeps_everything_and_fails(self):
        cases = {
            # Success with zero processes is impossible (prune itself runs).
            "empty-output-exit-0": "true",
            "blank-lines-only": "printf '\\n\\n'",
            "garbage-text": "echo 'this is not a process list'",
            "non-numeric-pid": f"echo 'abc {BEFORE} claude'",
            "non-numeric-start": "echo '9001 yesterday claude'",
            "missing-comm": f"echo '9001 {BEFORE}'",
            # One good line must not launder a bad one next to it.
            "good-line-plus-garbage": f"echo '{SELF_LINE}'; echo 'garbage line here now'",
        }
        for label, probe in cases.items():
            with self.subTest(label):
                old = self._old()
                out, err, rc = self._run(probe, {"backlog/0.3.21": SUPERSEDED_AT})
                self.assertTrue(
                    old.is_dir(),
                    f"{label}: undetermined must keep the dir. stdout={out!r} stderr={err!r}",
                )
                self.assertNotEqual(rc, 0, f"{label}: must exit non-zero. stdout={out!r}")

    # -- 6. RED today (kept half) ---------------------------------------
    def test_6_two_superseded_versions_split_by_one_claude_between(self):
        # 0.3.20 superseded at T1 (by 0.3.21), 0.3.21 superseded at T2 (by
        # 0.3.22). One claude started at T1 < start < T2: it began after 0.3.20
        # was already superseded (cannot be using it) but before 0.3.21 was.
        t1, t2 = 1_000_000, 2_000_000
        older = self._old("0.3.20")
        newer = self._old("0.3.21")
        probe = self._probe_printing(SELF_LINE, f"9001 {t1 + 500_000} claude")
        out, err, rc = self._run(
            probe, {"backlog/0.3.20": t1, "backlog/0.3.21": t2}
        )
        self.assertTrue(
            newer.is_dir(),
            f"0.3.21 is held by the claude that started before t2. rc={rc} "
            f"stdout={out!r} stderr={err!r}",
        )
        self.assertFalse(
            older.exists(),
            f"0.3.20 was superseded before that claude started; prune it. "
            f"rc={rc} stdout={out!r} stderr={err!r}",
        )
        self.assertIn("pruned backlog/0.3.20", out)
        self.assertTrue(self.cur.is_dir())


if __name__ == "__main__":
    unittest.main()
