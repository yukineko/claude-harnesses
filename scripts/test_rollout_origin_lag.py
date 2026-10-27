#!/usr/bin/env python3
"""RED repro: check-plugin-rollout.py does not flag a plugin whose source
version is ahead of the upstream (origin) branch.

Observed 2026-10-27: the yukineko marketplace is `source: directory` with
`autoUpdate: true`. Claude Code's auto-update reinstalled every plugin at the
version found at origin/HEAD (recorded gitCommitSha == origin/main), not the
working tree. local main was 12 commits ahead and unpushed, so exactly the two
plugins whose version differed main vs origin (overwatch 0.2.106 -> 0.2.105,
tdd 0.1.56 -> 0.1.54) were repointed to cache dirs holding only the launcher.
overwatch's SessionStart went dark and tdd's Stop gate blocked every stop.
rollout-plugins.sh had already deployed the newer versions; the revert undid
it silently.

Contract asserted here: when a plugin's plugin.json version at HEAD differs
from its version at the upstream branch, the rollout checker names that plugin
and says it is ahead of origin (so the reader pushes before trusting a
rollout). Control: same fixture with origin == HEAD names no such finding.

HERMETIC: temp git repos (a bare `origin` + a clone), temp registry/settings/
cache. The real ~/.claude is never read or written.
"""
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
CHECKER = HERE / "check-plugin-rollout.py"


def _git(cwd, *args):
    subprocess.run(
        ["git", "-c", "user.name=t", "-c", "user.email=t@t", "-c", "core.hooksPath=/dev/null", *args],
        cwd=cwd,
        check=True,
        capture_output=True,
        text=True,
    )


def _write_plugin(repo, version):
    pj = repo / "crates" / "alpha" / ".claude-plugin" / "plugin.json"
    pj.parent.mkdir(parents=True, exist_ok=True)
    pj.write_text(json.dumps({"name": "alpha", "version": version}))
    mk = repo / ".claude-plugin" / "marketplace.json"
    mk.parent.mkdir(parents=True, exist_ok=True)
    mk.write_text(json.dumps({"name": "yukineko", "plugins": [{"name": "alpha", "version": version}]}))


def _sandbox(tmp, origin_version, head_version):
    tmp = Path(tmp)
    bare, repo = tmp / "origin.git", tmp / "repo"
    _git(tmp, "init", "-q", "--bare", "-b", "main", str(bare))
    _git(tmp, "clone", "-q", str(bare), str(repo))
    _git(repo, "checkout", "-q", "-B", "main")
    _write_plugin(repo, origin_version)
    _git(repo, "add", "-A")
    _git(repo, "commit", "-q", "-m", "origin version")
    _git(repo, "push", "-q", "-u", "origin", "main")
    if head_version != origin_version:
        _write_plugin(repo, head_version)
        _git(repo, "commit", "-q", "-am", "bump, unpushed")

    cache = tmp / "cache"
    install = cache / "yukineko" / "alpha" / head_version
    (install / "bin").mkdir(parents=True)
    registry = tmp / "installed_plugins.json"
    registry.write_text(json.dumps({
        "version": 2,
        "plugins": {"alpha@yukineko": [{
            "scope": "user",
            "installPath": str(install),
            "version": head_version,
            "lastUpdated": "2026-10-27T00:00:00.000Z",
        }]},
    }))
    settings = tmp / "settings.json"
    settings.write_text(json.dumps({"enabledPlugins": {"alpha@yukineko": True}}))
    return repo, cache, registry, settings


def _run(repo, cache, registry, settings):
    env = dict(os.environ)
    env.update(
        CLAUDE_PLUGIN_REGISTRY=str(registry),
        CLAUDE_SETTINGS=str(settings),
        CLAUDE_PLUGIN_CACHE=str(cache / "yukineko"),
    )
    r = subprocess.run(
        [sys.executable, str(CHECKER)],
        cwd=repo, env=env, capture_output=True, text=True,
    )
    return r, r.stdout + r.stderr


def _origin_lag_lines(out):
    return [
        ln for ln in out.splitlines()
        if "alpha" in ln and "origin" in ln.lower() and "ahead" in ln.lower()
    ]


class RolloutCheckerFlagsUnpushedPluginVersion(unittest.TestCase):
    def test_source_version_ahead_of_origin_is_flagged(self):
        with tempfile.TemporaryDirectory() as t:
            r, out = _run(*_sandbox(t, origin_version="0.1.0", head_version="0.1.1"))
            self.assertTrue(
                _origin_lag_lines(out),
                "check-plugin-rollout.py did not report that alpha 0.1.1 is ahead of "
                "origin (0.1.0); marketplace autoUpdate will revert it to origin's "
                f"version.\nrc={r.returncode}\n{out}",
            )

    def test_control_origin_equal_to_head_is_not_flagged(self):
        """Control: a rule 'always say ahead of origin' would pass the test above."""
        with tempfile.TemporaryDirectory() as t:
            r, out = _run(*_sandbox(t, origin_version="0.1.1", head_version="0.1.1"))
            self.assertFalse(
                _origin_lag_lines(out),
                f"control failed: flagged alpha although origin == HEAD.\nrc={r.returncode}\n{out}",
            )


if __name__ == "__main__":
    unittest.main()
