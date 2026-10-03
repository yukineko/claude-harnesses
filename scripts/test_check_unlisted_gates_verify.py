#!/usr/bin/env python3
"""Independent verifier tests for scripts/check-unlisted-gates.py (666b6704).

Written by the condukt verifier, not the implementer. Every fixture is a
throwaway git repo in a temp dir with an isolated git config and HOME; nothing
reads or writes the real repo's config or hooks (the real .githooks are only
COPIED, read-only, into fixtures).

Contract checked (from the scanner's docstring):
  0 = every check-*.py / check-*.sh in scripts/ is invoked by an active hook
  1 = some are not; each is named
  2 = undetermined (never 0)
A mention in a comment, echo, heredoc or QUOTED STRING LITERAL is not an
invocation.
"""

import os
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

SRC = Path(__file__).resolve().parent.parent
SCANNER = SRC / "scripts" / "check-unlisted-gates.py"
HP_KEY = "core." + "hooks" + "Path"
CFG = "GIT_" + "CONFIG_"
DOTGIT = ".git"
REAL_HOOKS = Path(
    subprocess.run(
        ["git", "-C", str(SRC), "rev-parse", "--path-format=absolute", "--git-common-dir"],
        capture_output=True, text=True, check=True,
    ).stdout.strip()
).parent / ".githooks"
STUB = "#!/usr/bin/env python3\nraise SystemExit(0)\n"
RUN_DEF = 'REPO="$(git rev-parse --show-toplevel)"\nrun() {\n    python3 "$REPO/scripts/$1" || rc=1\n}\n'


class Repo:
    def __init__(self, test):
        self.root = Path(tempfile.mkdtemp(prefix="666b6704v-")).resolve()
        test.addCleanup(self.cleanup)
        self.env = {
            "PATH": "/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin",
            "HOME": str(self.root / "home"),
            "LC_ALL": "C",
            CFG + "NOSYSTEM": "1",
            CFG + "GLOBAL": os.devnull,
            "GIT_CEILING_DIRECTORIES": str(self.root.parent),
            "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@example.invalid",
            "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@example.invalid",
        }
        (self.root / "home").mkdir()
        self.top = self.root / "repo"
        self.top.mkdir()
        self.git("init", "-q", "-b", "main")
        (self.top / "scripts").mkdir()
        self.hooks = self.top / ".githooks"
        self.hooks.mkdir()

    def cleanup(self):
        for dp, dns, fns in os.walk(self.root):
            for n in dns + fns:
                try:
                    os.chmod(os.path.join(dp, n), 0o755)
                except OSError:
                    pass
        shutil.rmtree(self.root, ignore_errors=True)

    def git(self, *args, cwd=None, check=True):
        return subprocess.run(["git", *args], cwd=cwd or self.top, env=self.env,
                              capture_output=True, text=True, check=check)

    def set_hp(self, value):
        self.git("config", HP_KEY, value)

    def scanner(self, *names, where=None):
        d = (where or self.top) / "scripts"
        d.mkdir(exist_ok=True)
        for n in names:
            (d / n).write_text(STUB)

    def hook(self, name, body, mode=0o755, where=None):
        p = (where or self.hooks) / name
        p.write_text(body)
        os.chmod(p, mode)
        return p

    def scan(self, cwd=None):
        return subprocess.run([sys.executable, str(SCANNER)], cwd=cwd or self.top,
                              env=self.env, capture_output=True, text=True)


class T(unittest.TestCase):
    def setUp(self):
        self.r = Repo(self)

    def abs_path(self):
        self.r.set_hp(str(self.r.hooks))

    def commit_init(self):
        self.r.git("add", "-A")
        self.r.git("-c", HP_KEY + "=/nonexistent", "commit", "-qm", "init")

    # ---- mention vs invocation (probe 1) --------------------------------

    def assert_unlisted(self, body, name="check-b.py"):
        self.abs_path()
        self.r.scanner("check-a.py", name)
        self.r.hook("pre-commit", "#!/bin/sh\n" + RUN_DEF + "run check-a.py a\n" + body)
        p = self.r.scan()
        self.assertEqual(p.returncode, 1, p.stdout + p.stderr)
        self.assertIn(name, p.stderr)

    def test_comment_mention_is_not_invocation(self):
        self.assert_unlisted("# python3 scripts/check-b.py\n")

    def test_unquoted_echo_mention_is_not_invocation(self):
        self.assert_unlisted("echo python3 scripts/check-b.py >&2\n")

    def test_quoted_echo_mention_is_not_invocation(self):
        self.assert_unlisted('echo "  python3 scripts/check-b.py --fix" >&2\n')

    def test_heredoc_mention_is_not_invocation(self):
        self.assert_unlisted("cat >&2 <<'EOF'\npython3 scripts/check-b.py\nEOF\n")

    def test_multiline_double_quoted_mention_is_not_invocation(self):
        # Real shell never executes line 2: it is inside "...". The docstring
        # claims quoted string literals are excluded.
        self.assert_unlisted('echo "blocked. Fix with:\n    python3 scripts/check-b.py --update\n" >&2\n')

    def test_multiline_single_quoted_mention_is_not_invocation(self):
        self.assert_unlisted("printf '%s\\n' 'blocked. Fix with:\n    python3 scripts/check-b.py --update\n' >&2\n")

    def test_shell_confirms_multiline_quote_does_not_execute(self):
        # Ground truth for the two tests above, observed with /bin/sh.
        marker = self.r.root / "ran"
        (self.r.top / "scripts" / "check-b.py").write_text(
            "open(%r,'w').write('x')\n" % str(marker))
        script = 'echo "x\n    python3 scripts/check-b.py\n" >/dev/null\n'
        subprocess.run(["/bin/sh", "-c", script], cwd=self.r.top, check=True)
        self.assertFalse(marker.exists())

    def test_run_without_run_function_is_not_invocation(self):
        self.abs_path()
        self.r.scanner("check-a.py", "check-b.py")
        self.r.hook("pre-commit", "#!/bin/sh\npython3 scripts/check-a.py\nrun check-b.py b\n")
        p = self.r.scan()
        self.assertEqual(p.returncode, 1, p.stderr)
        self.assertIn("check-b.py", p.stderr)

    # ---- invocation forms (probe 2) -------------------------------------

    def test_invocation_forms_all_count(self):
        self.abs_path()
        names = ["check-run.py", "check-py.py", "check-sh.sh", "check-bash.sh",
                 "check-var.py", "check-cont.py", "check-direct.sh", "check-sub.py"]
        self.r.scanner(*names)
        body = ("#!/bin/sh\n" + RUN_DEF +
                "run check-run.py label\n"
                'python3 "$REPO/scripts/check-py.py" --x\n'
                "sh scripts/check-sh.sh\n"
                'bash -e "$REPO/scripts/check-bash.sh"\n'
                'SCANNER="$REPO/scripts/check-var.py"\npython3 "$SCANNER" --repo "$REPO"\n'
                '( cd "$REPO" && CARGO_TARGET_DIR=/x \\\n      python3 scripts/check-cont.py >&2 )\n'
                "./scripts/check-direct.sh\n"
                'out="$(python3 scripts/check-sub.py 2>&1)"\n')
        self.r.hook("pre-commit", body)
        p = self.r.scan()
        self.assertEqual(p.returncode, 0, p.stderr)

    def test_real_hooks_count_every_scanner_they_run(self):
        # Copy the real hooks (read-only source) into the fixture.
        for h in REAL_HOOKS.iterdir():
            if h.is_file():
                shutil.copy2(h, self.r.hooks / h.name)
        self.abs_path()
        invoked = [
            "check-prompt-injection.py", "check-fail-open.py", "check-fail-open-diff.py",
            "check-doc-claims.py", "check-claudemd-claims.py", "check-plugin-versions.py",
            "check-version-bumped.py", "check-hardcoded-secret.py", "check-raw-io-ratchet.py",
            "check-fault-injection-adoption.py", "check-worktree-isolation.py",
            "check-gate-crates-sync.py", "check-gate-protection.py",
            "check-cross-crate-constants.py", "check-launcher-exec-bit.py",
            "check-clippy-lints.py", "check-test-weakening.py",
            "check-plugin-rollout.py", "check-compile-fail.py",
        ]
        self.r.scanner(*invoked)
        p = self.r.scan()
        self.assertEqual(p.returncode, 0, p.stderr)
        self.r.scanner("check-new.py")
        p = self.r.scan()
        self.assertEqual(p.returncode, 1, p.stderr)
        named = [l.strip() for l in p.stderr.splitlines() if l.startswith("  scripts/")]
        self.assertEqual(named, ["scripts/check-new.py"])

    # ---- hook path resolution ------------------------------------------

    def test_absolute_hookspath_from_linked_worktree(self):
        self.r.scanner("check-a.py")
        self.r.hook("pre-commit", "#!/bin/sh\n" + RUN_DEF + "run check-a.py a\n")
        self.commit_init()
        self.abs_path()
        wt = self.r.root / "wt"
        self.r.git("worktree", "add", "-q", "-b", "w", str(wt))
        self.assertEqual(self.r.scan(cwd=wt).returncode, 0)
        self.r.scanner("check-new.py", where=wt)
        # Wired only in the worktree's own .githooks: irrelevant, the path is main's.
        self.r.hook("pre-commit", "#!/bin/sh\n" + RUN_DEF + "run check-a.py a\nrun check-new.py n\n",
                    where=wt / ".githooks")
        p = self.r.scan(cwd=wt)
        self.assertEqual(p.returncode, 1, p.stderr)
        self.assertIn("check-new.py", p.stderr)

    def test_relative_hookspath_resolves_per_worktree(self):
        self.r.scanner("check-a.py")
        self.r.hook("pre-commit", "#!/bin/sh\n" + RUN_DEF + "run check-a.py a\n")
        self.commit_init()
        self.r.set_hp(".githooks")
        wt = self.r.root / "wt"
        self.r.git("worktree", "add", "-q", "-b", "w", str(wt))
        self.r.scanner("check-new.py", where=wt)
        p = self.r.scan(cwd=wt)
        self.assertEqual(p.returncode, 1, p.stderr)
        self.r.hook("pre-commit", "#!/bin/sh\n" + RUN_DEF + "run check-a.py a\nrun check-new.py n\n",
                    where=wt / ".githooks")
        p = self.r.scan(cwd=wt)
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertEqual(self.r.scan().returncode, 0)

    def test_unset_hookspath_uses_common_dir_hooks(self):
        self.r.scanner("check-a.py", "check-b.py")
        gh = self.r.top / DOTGIT / "hooks"
        for f in gh.iterdir():
            f.unlink()
        self.r.hook("pre-commit", "#!/bin/sh\n" + RUN_DEF + "run check-a.py a\n", where=gh)
        p = self.r.scan()
        self.assertEqual(p.returncode, 1, p.stderr)
        self.assertIn("check-b.py", p.stderr)
        self.r.hook("pre-push", "#!/bin/sh\npython3 scripts/check-b.py\n", where=gh)
        self.assertEqual(self.r.scan().returncode, 0)
        # and from a linked worktree, the COMMON dir's hooks are read
        self.commit_init()
        wt = self.r.root / "wt"
        self.r.git("worktree", "add", "-q", "-b", "w", str(wt))
        self.assertEqual(self.r.scan(cwd=wt).returncode, 0)

    # ---- non-executable hooks (probe 3) --------------------------------

    def test_git_skips_non_executable_hook_and_scanner_agrees(self):
        self.abs_path()
        marker = self.r.root / "commit-msg-ran"
        self.r.hook("pre-commit", "#!/bin/sh\n" + RUN_DEF + "run check-a.py a\n")
        self.r.hook("commit-msg", "#!/bin/sh\ntouch %s\npython3 scripts/check-b.py\n" % marker, mode=0o644)
        self.r.scanner("check-a.py", "check-b.py")
        (self.r.top / "f").write_text("x")
        self.r.git("add", "f")
        c = self.r.git("commit", "-qm", "m")
        self.assertFalse(marker.exists(), "git ran a non-executable hook")
        self.assertIn("not set as executable", c.stderr + c.stdout)
        p = self.r.scan()
        self.assertEqual(p.returncode, 1, p.stderr)
        self.assertIn("check-b.py", p.stderr)
        # core.fileMode=false does not change it: the hook's bit is on the fs.
        self.r.git("config", "core.fileMode", "false")
        (self.r.top / "g").write_text("x")
        self.r.git("add", "g")
        self.r.git("commit", "-qm", "m2")
        self.assertFalse(marker.exists())

    # ---- undetermined (probe 6) ----------------------------------------

    def assert_undetermined(self, p):
        self.assertNotEqual(p.returncode, 0, p.stdout + p.stderr)
        self.assertEqual(p.returncode, 2, p.stdout + p.stderr)

    def base(self):
        self.abs_path()
        self.r.scanner("check-a.py")
        return self.r.hook("pre-commit", "#!/bin/sh\n" + RUN_DEF + "run check-a.py a\n")

    def test_not_a_git_repo(self):
        d = self.r.root / "plain"
        d.mkdir()
        self.assert_undetermined(self.r.scan(cwd=d))

    def test_hookspath_setting_unreadable(self):
        self.base()
        cfg = self.r.top / DOTGIT / "config"
        cfg.write_text(cfg.read_text() + "\n[broken\n")
        self.assert_undetermined(self.r.scan())

    def test_unreadable_pre_commit(self):
        pc = self.base()
        os.chmod(pc, 0o311)
        self.assert_undetermined(self.r.scan())

    def test_missing_pre_commit(self):
        self.base().unlink()
        self.assert_undetermined(self.r.scan())

    def test_hookspath_dir_missing(self):
        self.base()
        self.r.set_hp(str(self.r.root / "nope"))
        self.assert_undetermined(self.r.scan())

    def test_empty_scripts_dir(self):
        self.base()
        (self.r.top / "scripts" / "check-a.py").unlink()
        self.assert_undetermined(self.r.scan())

    def test_missing_scripts_dir(self):
        self.base()
        shutil.rmtree(self.r.top / "scripts")
        self.assert_undetermined(self.r.scan())

    def test_unreadable_scripts_dir(self):
        self.base()
        os.chmod(self.r.top / "scripts", 0o000)
        self.assert_undetermined(self.r.scan())

    def test_zero_invocations(self):
        self.base()
        self.r.hook("pre-commit", "#!/bin/sh\necho hi\n# python3 scripts/check-a.py\n")
        self.assert_undetermined(self.r.scan())

    # ---- speed (criterion e) -------------------------------------------

    def test_real_repo_fast(self):
        t0 = time.monotonic()
        p = subprocess.run([sys.executable, str(SCANNER)], cwd=SRC, capture_output=True, text=True)
        self.assertLess(time.monotonic() - t0, 1.0)
        self.assertIn(p.returncode, (0, 1), p.stderr)


if __name__ == "__main__":
    unittest.main()
