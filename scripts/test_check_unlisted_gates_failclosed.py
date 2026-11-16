#!/usr/bin/env python3
"""Fail-closed / red-direction contract of check-unlisted-gates' settings.json chain (96e2fde0).

Written by an independent verifier against the scanner's docstring, NOT against
its implementation. The docstring (section "Claude Code hooks and invocation
chains") promises:

  * settings.json unreadable / not JSON / malformed `hooks` shape, a command
    that cannot be parsed, a reached script that cannot be read or parsed
    -> undetermined (exit 2);
  * settings.json missing -> no project hooks (absence, not undetermined);
  * `disableAllHooks: true` -> no Claude hook counted;
  * os.system / shell=True strings / mere mentions / calls inside functions
    nobody calls are NOT followed -> the target reads as unlisted (red).

Every case runs the real scanner (subprocess) against a throwaway git repo that
is a COPY of this checkout's .githooks/ and scripts/, with .claude/settings.json
written per case. check-plugin-rollout.py is reached ONLY through the
SessionStart chain (settings.json -> session-worktree-init.py ->
session-rollout-drift.py -> check-plugin-rollout.py), so it is the probe for
"was the Claude hook chain counted". Nothing here writes to the real repository.
"""

import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SRC = Path(__file__).resolve().parent.parent
SCANNER = SRC / "scripts" / "check-unlisted-gates.py"
ROLLOUT = "check-plugin-rollout.py"
REAL_SETTINGS = json.loads((SRC / ".claude" / "settings.json").read_text())


def _with_session_hook(script):
    """Real settings.json plus one extra SessionStart command hook running scripts/<script>."""
    s = json.loads(json.dumps(REAL_SETTINGS))
    s["hooks"].setdefault("SessionStart", []).append({
        "hooks": [{
            "type": "command",
            "command": 'python3 "$CLAUDE_PROJECT_DIR/scripts/%s"' % script,
        }]
    })
    return json.dumps(s, indent=2)


class Base(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.root = Path(tempfile.mkdtemp(prefix="unlisted-failclosed-")).resolve()
        cls.template = cls.root / "template"
        shutil.copytree(SRC / "scripts", cls.template / "scripts", symlinks=True,
                        ignore=shutil.ignore_patterns("__pycache__", "*.pyc"))
        shutil.copytree(SRC / ".githooks", cls.template / ".githooks", symlinks=True)
        cls.env = dict(
            PATH="/usr/bin:/bin:/usr/sbin:/sbin",
            HOME=str(cls.root),
            LC_ALL="C",
            GIT_CONFIG_NOSYSTEM="1",
            GIT_CONFIG_GLOBAL=os.devnull,
            GIT_CEILING_DIRECTORIES=str(cls.root),
        )
        cls.n = 0

    @classmethod
    def tearDownClass(cls):
        for p in cls.root.rglob("*"):
            try:
                if not p.is_symlink():
                    p.chmod(0o755 if p.is_dir() else 0o644)
            except OSError:
                pass
        shutil.rmtree(cls.root, True)

    def scan(self, settings=None, extra_scripts=None):
        """Run the scanner. settings: None = no settings.json, else its exact text.

        extra_scripts: {name: text} written into the copied scripts/.
        Returns (rc, unlisted set, detail string).
        """
        type(self).n += 1
        repo = self.root / ("repo%d" % self.n)
        shutil.copytree(self.template, repo, symlinks=True)
        subprocess.run(["git", "init", "-q", "-b", "main"], cwd=repo,
                       env=self.env, check=True, capture_output=True)
        if settings is not None:
            (repo / ".claude").mkdir()
            (repo / ".claude" / "settings.json").write_text(settings)
        for name, text in (extra_scripts or {}).items():
            (repo / "scripts" / name).write_text(text)
        p = subprocess.run(
            [sys.executable, str(SCANNER), "--hooks-dir", str(repo / ".githooks")],
            cwd=repo, env=self.env, capture_output=True, text=True, timeout=120,
        )
        unlisted = {
            line.strip()[len("scripts/"):]
            for line in p.stderr.splitlines()
            if line.startswith("  scripts/")
        }
        detail = "rc=%d\nstdout:\n%s\nstderr:\n%s" % (p.returncode, p.stdout, p.stderr)
        return p.returncode, unlisted, detail, repo


class Undetermined(Base):
    """Docstring: these inputs are undetermined (exit 2), never a verdict."""

    def assertUndetermined(self, rc, unlisted, detail):
        self.assertEqual(rc, 2, detail)
        self.assertIn("UNDETERMINED", detail, detail)
        self.assertEqual(unlisted, set(), detail)

    def test_1_settings_not_json(self):
        rc, unlisted, detail, _ = self.scan(settings="{bad")
        self.assertUndetermined(rc, unlisted, detail)

    def test_2_hooks_event_not_a_list(self):
        rc, unlisted, detail, _ = self.scan(settings='{"hooks":{"SessionStart":"x"}}')
        self.assertUndetermined(rc, unlisted, detail)

    def test_2b_hooks_not_an_object(self):
        rc, unlisted, detail, _ = self.scan(settings='{"hooks":["x"]}')
        self.assertUndetermined(rc, unlisted, detail)

    def test_2c_group_without_hooks_list(self):
        rc, unlisted, detail, _ = self.scan(settings='{"hooks":{"SessionStart":[{"hooks":"x"}]}}')
        self.assertUndetermined(rc, unlisted, detail)

    def test_2d_command_hook_without_string_command(self):
        rc, unlisted, detail, _ = self.scan(
            settings='{"hooks":{"SessionStart":[{"hooks":[{"type":"command","command":1}]}]}}')
        self.assertUndetermined(rc, unlisted, detail)

    def test_2e_settings_not_an_object(self):
        rc, unlisted, detail, _ = self.scan(settings="[]")
        self.assertUndetermined(rc, unlisted, detail)

    def test_2f_hook_command_unparseable(self):
        s = json.loads(json.dumps(REAL_SETTINGS))
        s["hooks"]["SessionStart"].append(
            {"hooks": [{"type": "command", "command": 'python3 "$CLAUDE_PROJECT_DIR/scripts/x.py'}]})
        rc, unlisted, detail, _ = self.scan(settings=json.dumps(s))
        self.assertUndetermined(rc, unlisted, detail)

    def test_2g_settings_unreadable(self):
        if os.geteuid() == 0:
            self.skipTest("root ignores file modes")
        # Build the repo via scan()'s layout, then chmod 000 and rerun.
        rc0, _, detail0, repo = self.scan(settings=json.dumps(REAL_SETTINGS))
        self.assertIn(rc0, (0, 1), detail0)
        (repo / ".claude" / "settings.json").chmod(0)
        p = subprocess.run(
            [sys.executable, str(SCANNER), "--hooks-dir", str(repo / ".githooks")],
            cwd=repo, env=self.env, capture_output=True, text=True, timeout=120,
        )
        detail = "rc=%d\nstderr:\n%s" % (p.returncode, p.stderr)
        self.assertEqual(p.returncode, 2, detail)

    def test_5_chained_python_script_syntax_error(self):
        rc, unlisted, detail, _ = self.scan(
            settings=_with_session_hook("zz-chain-broken.py"),
            extra_scripts={"zz-chain-broken.py": "def broken(:\n    pass\n"},
        )
        self.assertUndetermined(rc, unlisted, detail)

    def test_5b_real_chain_link_syntax_error(self):
        # Break a link of the real SessionStart chain (session-rollout-drift.py).
        rc, unlisted, detail, _ = self.scan(
            settings=json.dumps(REAL_SETTINGS),
            extra_scripts={"session-rollout-drift.py": "CHECKER = (\n"},
        )
        self.assertUndetermined(rc, unlisted, detail)

    def test_5c_chained_shell_script_unparseable(self):
        rc, unlisted, detail, _ = self.scan(
            settings=_with_session_hook("zz-chain-broken.sh").replace("python3 ", "bash "),
            extra_scripts={"zz-chain-broken.sh": 'echo "unterminated\n'},
        )
        self.assertUndetermined(rc, unlisted, detail)

    def test_5d_chained_script_unreadable(self):
        if os.geteuid() == 0:
            self.skipTest("root ignores file modes")
        rc0, _, detail0, repo = self.scan(settings=json.dumps(REAL_SETTINGS))
        self.assertIn(rc0, (0, 1), detail0)
        (repo / "scripts" / "session-rollout-drift.py").chmod(0)
        p = subprocess.run(
            [sys.executable, str(SCANNER), "--hooks-dir", str(repo / ".githooks")],
            cwd=repo, env=self.env, capture_output=True, text=True, timeout=120,
        )
        detail = "rc=%d\nstderr:\n%s" % (p.returncode, p.stderr)
        self.assertEqual(p.returncode, 2, detail)


class RedDirection(Base):
    """Docstring: absence / disabled / unfollowed shapes leave the target unlisted."""

    def test_3_settings_missing_rollout_unlisted(self):
        rc, unlisted, detail, _ = self.scan(settings=None)
        self.assertEqual(rc, 1, detail)
        self.assertIn(ROLLOUT, unlisted, detail)

    def test_4_disable_all_hooks_rollout_unlisted(self):
        s = json.loads(json.dumps(REAL_SETTINGS))
        s["disableAllHooks"] = True
        rc, unlisted, detail, _ = self.scan(settings=json.dumps(s))
        self.assertEqual(rc, 1, detail)
        self.assertIn(ROLLOUT, unlisted, detail)

    def test_4b_settings_local_json_not_read(self):
        # Only settings.local.json carries the hook: still unlisted.
        rc, unlisted, detail, repo = self.scan(settings=None)
        self.assertIn(ROLLOUT, unlisted, detail)
        (repo / ".claude").mkdir()
        (repo / ".claude" / "settings.local.json").write_text(json.dumps(REAL_SETTINGS))
        p = subprocess.run(
            [sys.executable, str(SCANNER), "--hooks-dir", str(repo / ".githooks")],
            cwd=repo, env=self.env, capture_output=True, text=True, timeout=120,
        )
        unlisted2 = {l.strip()[len("scripts/"):] for l in p.stderr.splitlines()
                     if l.startswith("  scripts/")}
        self.assertEqual(p.returncode, 1, p.stderr)
        self.assertIn(ROLLOUT, unlisted2, p.stderr)

    def _target_unlisted(self, variant, body):
        target = "check-zz-target-%s.py" % variant
        helper = "zz-chain-%s.py" % variant
        rc, unlisted, detail, _ = self.scan(
            settings=_with_session_hook(helper),
            extra_scripts={
                helper: body.replace("TARGET", target),
                target: "#!/usr/bin/env python3\nraise SystemExit(0)\n",
            },
        )
        self.assertEqual(rc, 1, detail)
        return target, unlisted, detail

    def test_6a_os_system_not_followed(self):
        target, unlisted, detail = self._target_unlisted(
            "ossystem", 'import os\nos.system("python3 scripts/TARGET")\n')
        self.assertIn(target, unlisted, detail)

    def test_6b_shell_true_not_followed(self):
        target, unlisted, detail = self._target_unlisted(
            "shelltrue",
            'import subprocess\nsubprocess.run("python3 scripts/TARGET", shell=True)\n')
        self.assertIn(target, unlisted, detail)

    def test_6c_string_mention_not_followed(self):
        target, unlisted, detail = self._target_unlisted(
            "mention",
            'import sys\nMSG = "run python3 scripts/TARGET to fix"\nprint(MSG, file=sys.stderr)\n')
        self.assertIn(target, unlisted, detail)

    def test_6d_call_in_uncalled_function_not_followed(self):
        target, unlisted, detail = self._target_unlisted(
            "uncalled",
            "import subprocess, sys\n"
            "def never():\n"
            "    subprocess.run([sys.executable, 'scripts/TARGET'])\n"
            "def main():\n"
            "    return 0\n"
            "raise SystemExit(main())\n")
        self.assertIn(target, unlisted, detail)

    def test_6e_os_execv_not_followed(self):
        target, unlisted, detail = self._target_unlisted(
            "execv",
            "import os, sys\nos.execv(sys.executable, [sys.executable, 'scripts/TARGET'])\n")
        self.assertIn(target, unlisted, detail)

    def test_6_control_real_subprocess_is_followed(self):
        # Guards 6a-6e from vacuity: the same fixture with a real argv call IS counted.
        target = "check-zz-target-positive.py"
        helper = "zz-chain-positive.py"
        rc, unlisted, detail, _ = self.scan(
            settings=_with_session_hook(helper),
            extra_scripts={
                helper: "import subprocess, sys\n"
                        "def main():\n"
                        "    subprocess.run([sys.executable, 'scripts/%s'])\n"
                        "main()\n" % target,
                target: "#!/usr/bin/env python3\nraise SystemExit(0)\n",
            },
        )
        self.assertIn(rc, (0, 1), detail)
        self.assertNotIn(target, unlisted, detail)

    def test_7_control_real_settings_rollout_listed(self):
        rc, unlisted, detail, _ = self.scan(settings=json.dumps(REAL_SETTINGS))
        self.assertIn(rc, (0, 1), detail)
        self.assertNotIn(ROLLOUT, unlisted, detail)


if __name__ == "__main__":
    unittest.main()
