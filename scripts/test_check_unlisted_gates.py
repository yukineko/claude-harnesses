#!/usr/bin/env python3
"""Tests for scripts/check-unlisted-gates.py (backlog 666b6704).

core.hooksPath is shared by every linked worktree. When it is the main tree's
ABSOLUTE .githooks, the hook LIST comes from main while the scanner BODIES come
from the worktree, so a scanner added in a worktree is silently never run (dark,
not red). The ruling keeps the absolute path and makes the asymmetry RED: a
`check-*.py` / `check-*.sh` present in the checkout's scripts/ that no active
hook invokes must fail the check.

Every fixture is a throwaway git repo under tempfile with an isolated git config
(no system/global config, ceiling dirs), so nothing here reads or writes the
real repository's config.

Exit-code contract asserted here: 0 only for "every scanner is invoked"; 1 for
"named scanners are unlisted"; any other non-zero for "could not determine".
Tests that need "non-zero" assert `!= 0`, and the undetermined ones additionally
assert it is not the clean code.
"""

import os
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SRC = Path(__file__).resolve().parent.parent
SCANNER = SRC / "scripts" / "check-unlisted-gates.py"

PRE_COMMIT = """#!/bin/sh
set -u
REPO="$(git rev-parse --show-toplevel)"
rc=0
run() {
    scanner="$1"
    path="$REPO/scripts/$scanner"
    python3 "$path" || rc=1
}
%s
exit "$rc"
"""

STUB = "#!/usr/bin/env python3\nraise SystemExit(0)\n"


def hook(lines):
    return PRE_COMMIT % "\n".join(lines)


class Fx:
    """A fixture repo with .githooks/ and scripts/; hooksPath configurable."""

    def __init__(self, test):
        self.test = test
        self.root = Path(tempfile.mkdtemp(prefix="666b6704-unlisted-")).resolve()
        test.addCleanup(self._cleanup)
        self.env = dict(
            PATH="/usr/bin:/bin:/usr/sbin:/sbin",
            HOME=str(self.root),
            LC_ALL="C",
            GIT_CONFIG_NOSYSTEM="1",
            GIT_CONFIG_GLOBAL=os.devnull,
            GIT_CEILING_DIRECTORIES=str(self.root.parent),
            GIT_AUTHOR_NAME="t",
            GIT_AUTHOR_EMAIL="t@example.invalid",
            GIT_COMMITTER_NAME="t",
            GIT_COMMITTER_EMAIL="t@example.invalid",
        )
        self.main = self.root / "main"
        self.main.mkdir()
        self.git(self.main, "init", "-q", "-b", "main")
        self.git(self.main, "config", "commit.gpgsign", "false")
        (self.main / ".githooks").mkdir()
        (self.main / "scripts").mkdir()

    def _cleanup(self):
        # Restore permissions removed by the unreadable-path tests so rmtree works.
        for dirpath, dirnames, filenames in os.walk(self.root):
            for n in dirnames + filenames:
                p = os.path.join(dirpath, n)
                try:
                    os.chmod(p, 0o755)
                except OSError:
                    pass
        shutil.rmtree(self.root, True)

    def git(self, cwd, *args, check=True):
        p = subprocess.run(["git", *args], cwd=cwd, env=self.env, capture_output=True, text=True)
        if check and p.returncode != 0:
            raise AssertionError("git %s failed (%d)\n%s%s" % (args, p.returncode, p.stdout, p.stderr))
        return p

    def write_hook(self, tree, name, body, executable=True):
        h = tree / ".githooks" / name
        h.write_text(body)
        h.chmod(0o755 if executable else 0o644)
        return h

    def add_scanner(self, tree, name):
        (tree / "scripts" / name).write_text(STUB)

    def set_hooks_path(self, mode):
        hp = str(self.main / ".githooks") if mode == "absolute" else ".githooks"
        self.git(self.main, "config", "core.hooksPath", hp)

    def commit_all(self, tree, msg="seed"):
        self.git(tree, "add", "-A")
        self.git(tree, "commit", "-q", "--no-verify", "-m", msg)

    def worktree(self):
        wt = self.root / "wt"
        self.git(self.main, "worktree", "add", "-q", "-b", "feat", str(wt))
        return wt

    def check(self, cwd):
        return subprocess.run(
            [sys.executable, str(SCANNER)], cwd=cwd, env=self.env, capture_output=True, text=True
        )


def out(p):
    return "rc=%d\nstdout:\n%s\nstderr:\n%s" % (p.returncode, p.stdout, p.stderr)


class Listed(unittest.TestCase):
    def test_fully_listed_tree_is_clean_and_names_count(self):
        fx = Fx(self)
        fx.write_hook(fx.main, "pre-commit", hook(["run check-a.py a", "run check-b.sh b"]))
        fx.add_scanner(fx.main, "check-a.py")
        fx.add_scanner(fx.main, "check-b.sh")
        fx.set_hooks_path("absolute")
        p = fx.check(fx.main)
        self.assertEqual(p.returncode, 0, out(p))
        lines = [l for l in p.stdout.splitlines() if l.strip()]
        self.assertEqual(len(lines), 1, out(p))
        self.assertIn("OK", lines[0])
        self.assertIn("2", lines[0])

    def test_non_scanner_files_in_scripts_are_not_counted(self):
        fx = Fx(self)
        fx.write_hook(fx.main, "pre-commit", hook(["run check-a.py a"]))
        fx.add_scanner(fx.main, "check-a.py")
        # Not gate scanners by the naming rule: helpers, tests, baselines, nested.
        fx.add_scanner(fx.main, "gate-bypass.py")
        fx.add_scanner(fx.main, "test_check_a.py")
        (fx.main / "scripts" / "check-a.baseline").write_text("x\n")
        (fx.main / "scripts" / "tests").mkdir()
        (fx.main / "scripts" / "tests" / "check-nested.sh").write_text(STUB)
        fx.set_hooks_path("absolute")
        p = fx.check(fx.main)
        self.assertEqual(p.returncode, 0, out(p))

    def test_scanner_invoked_only_from_pre_push_counts_as_listed(self):
        fx = Fx(self)
        fx.write_hook(fx.main, "pre-commit", hook(["run check-a.py a"]))
        fx.write_hook(
            fx.main,
            "pre-push",
            "#!/bin/sh\nif [ -f scripts/check-push.py ]; then\n"
            "    out=\"$(python3 scripts/check-push.py 2>&1)\"\nfi\nexit 0\n",
        )
        fx.add_scanner(fx.main, "check-a.py")
        fx.add_scanner(fx.main, "check-push.py")
        fx.set_hooks_path("absolute")
        p = fx.check(fx.main)
        self.assertEqual(p.returncode, 0, out(p))

    def test_scanner_invoked_via_variable_in_commit_msg_counts_as_listed(self):
        fx = Fx(self)
        fx.write_hook(fx.main, "pre-commit", hook(["run check-a.py a"]))
        fx.write_hook(
            fx.main,
            "commit-msg",
            '#!/bin/sh\nREPO="$(git rev-parse --show-toplevel)"\n'
            'SCANNER="$REPO/scripts/check-msg.py"\n'
            'python3 "$SCANNER" --pending-msg "$1"\n',
        )
        fx.add_scanner(fx.main, "check-a.py")
        fx.add_scanner(fx.main, "check-msg.py")
        fx.set_hooks_path("absolute")
        p = fx.check(fx.main)
        self.assertEqual(p.returncode, 0, out(p))


class Unlisted(unittest.TestCase):
    def test_unlisted_scanner_is_red_and_named(self):
        fx = Fx(self)
        fx.write_hook(fx.main, "pre-commit", hook(["run check-a.py a"]))
        fx.add_scanner(fx.main, "check-a.py")
        fx.add_scanner(fx.main, "check-new.py")
        fx.set_hooks_path("absolute")
        p = fx.check(fx.main)
        self.assertNotEqual(p.returncode, 0, out(p))
        self.assertIn("check-new.py", p.stdout + p.stderr)
        self.assertNotIn("check-a.py", (p.stdout + p.stderr).replace("check-new.py", ""), out(p))

    def test_absolute_hookspath_worktree_new_scanner_is_red(self):
        # The 666b6704 scenario: the worktree wires a new gate into ITS OWN
        # pre-commit and adds the scanner; the active hook is main's.
        fx = Fx(self)
        fx.write_hook(fx.main, "pre-commit", hook(["run check-a.py a"]))
        fx.add_scanner(fx.main, "check-a.py")
        fx.set_hooks_path("absolute")
        fx.commit_all(fx.main)
        wt = fx.worktree()
        fx.write_hook(wt, "pre-commit", hook(["run check-a.py a", "run check-brand-new.py brand-new"]))
        fx.add_scanner(wt, "check-brand-new.py")
        p = fx.check(wt)
        self.assertNotEqual(p.returncode, 0, out(p))
        self.assertIn("check-brand-new.py", p.stdout + p.stderr)

    def test_relative_hookspath_worktree_new_scanner_wired_is_clean(self):
        # Relative hooksPath resolves against the worktree's toplevel, so the
        # worktree's own pre-commit is the active hook and it lists the gate.
        fx = Fx(self)
        fx.write_hook(fx.main, "pre-commit", hook(["run check-a.py a"]))
        fx.add_scanner(fx.main, "check-a.py")
        fx.set_hooks_path("relative")
        fx.commit_all(fx.main)
        wt = fx.worktree()
        fx.write_hook(wt, "pre-commit", hook(["run check-a.py a", "run check-brand-new.py brand-new"]))
        fx.add_scanner(wt, "check-brand-new.py")
        p = fx.check(wt)
        self.assertEqual(p.returncode, 0, out(p))

    def test_relative_hookspath_worktree_new_scanner_unwired_is_red(self):
        fx = Fx(self)
        fx.write_hook(fx.main, "pre-commit", hook(["run check-a.py a"]))
        fx.add_scanner(fx.main, "check-a.py")
        fx.set_hooks_path("relative")
        fx.commit_all(fx.main)
        wt = fx.worktree()
        fx.add_scanner(wt, "check-brand-new.py")
        p = fx.check(wt)
        self.assertNotEqual(p.returncode, 0, out(p))
        self.assertIn("check-brand-new.py", p.stdout + p.stderr)

    def test_mention_in_comment_or_heredoc_or_echo_is_not_an_invocation(self):
        fx = Fx(self)
        body = hook(
            [
                "run check-a.py a",
                "# run check-c.py c",
                "#   python3 scripts/check-d.py",
                "echo \"hint: python3 scripts/check-e.py --update-baseline\" >&2",
                "cat >&2 <<'EOF'",
                "run check-f.py f",
                "    python3 scripts/check-f.py --update-baseline",
                "EOF",
            ]
        )
        fx.write_hook(fx.main, "pre-commit", body)
        for n in ("check-a.py", "check-c.py", "check-d.py", "check-e.py", "check-f.py"):
            fx.add_scanner(fx.main, n)
        fx.set_hooks_path("absolute")
        p = fx.check(fx.main)
        self.assertNotEqual(p.returncode, 0, out(p))
        text = p.stdout + p.stderr
        for n in ("check-c.py", "check-d.py", "check-e.py", "check-f.py"):
            self.assertIn(n, text, out(p))

    def test_multiline_quoted_mentions_are_not_invocations(self):
        # Quote state must carry across lines: the continuation line of a
        # multi-line "..." or '...' string is data, not a command.
        fx = Fx(self)
        body = hook(
            [
                "run check-a.py a",
                'echo "blocked. Fix with:',
                '    python3 scripts/check-b.py --update',
                '" >&2',
                "printf '%s\\n' 'or:",
                "    python3 scripts/check-c.py",
                "' >&2",
                'x="first; ${HOME:-}',
                '    python3 scripts/check-d.py"',
            ]
        )
        fx.write_hook(fx.main, "pre-commit", body)
        for n in ("check-a.py", "check-b.py", "check-c.py", "check-d.py"):
            fx.add_scanner(fx.main, n)
        fx.set_hooks_path("absolute")
        p = fx.check(fx.main)
        self.assertEqual(p.returncode, 1, out(p))
        for n in ("check-b.py", "check-c.py", "check-d.py"):
            self.assertIn(n, p.stdout + p.stderr)

    def test_command_substitution_inside_double_quotes_counts(self):
        # "$(...)" and `...` inside double quotes really execute.
        fx = Fx(self)
        body = hook(
            [
                "run check-a.py a",
                'msg="result:',
                '$(python3 scripts/check-b.py)"',
                'echo "also `sh scripts/check-c.sh`"',
            ]
        )
        fx.write_hook(fx.main, "pre-commit", body)
        for n in ("check-a.py", "check-b.py", "check-c.sh"):
            fx.add_scanner(fx.main, n)
        fx.set_hooks_path("absolute")
        p = fx.check(fx.main)
        self.assertEqual(p.returncode, 0, out(p))

    def test_invocation_from_non_executable_hook_does_not_count(self):
        # git does not run a hook without the exec bit.
        fx = Fx(self)
        fx.write_hook(fx.main, "pre-commit", hook(["run check-a.py a"]))
        fx.write_hook(fx.main, "pre-push", "#!/bin/sh\npython3 scripts/check-push.py\n", executable=False)
        fx.add_scanner(fx.main, "check-a.py")
        fx.add_scanner(fx.main, "check-push.py")
        fx.set_hooks_path("absolute")
        p = fx.check(fx.main)
        self.assertNotEqual(p.returncode, 0, out(p))
        self.assertIn("check-push.py", p.stdout + p.stderr)

    def test_unset_hookspath_reads_git_common_dir_hooks(self):
        fx = Fx(self)
        hooks = fx.main / ".git" / "hooks"
        hooks.mkdir(exist_ok=True)
        h = hooks / "pre-commit"
        h.write_text(hook(["run check-a.py a"]))
        h.chmod(0o755)
        fx.add_scanner(fx.main, "check-a.py")
        p = fx.check(fx.main)
        self.assertEqual(p.returncode, 0, out(p))
        fx.add_scanner(fx.main, "check-b.py")
        p = fx.check(fx.main)
        self.assertNotEqual(p.returncode, 0, out(p))
        self.assertIn("check-b.py", p.stdout + p.stderr)


class Undetermined(unittest.TestCase):
    def assertUndetermined(self, p):
        self.assertNotEqual(p.returncode, 0, out(p))
        self.assertNotIn("OK", p.stdout, out(p))

    def test_missing_pre_commit_is_undetermined(self):
        fx = Fx(self)
        fx.add_scanner(fx.main, "check-a.py")
        fx.set_hooks_path("absolute")
        self.assertUndetermined(fx.check(fx.main))

    def test_unreadable_hook_body_is_undetermined(self):
        fx = Fx(self)
        h = fx.write_hook(fx.main, "pre-commit", hook(["run check-a.py a"]))
        fx.add_scanner(fx.main, "check-a.py")
        fx.set_hooks_path("absolute")
        h.chmod(0o111)
        self.assertFalse(os.access(h, os.R_OK), "precondition: hook body unreadable")
        self.assertUndetermined(fx.check(fx.main))

    def test_unreadable_sibling_hook_is_undetermined(self):
        fx = Fx(self)
        fx.write_hook(fx.main, "pre-commit", hook(["run check-a.py a"]))
        pp = fx.write_hook(fx.main, "pre-push", "#!/bin/sh\npython3 scripts/check-push.py\n")
        fx.add_scanner(fx.main, "check-a.py")
        fx.add_scanner(fx.main, "check-push.py")
        fx.set_hooks_path("absolute")
        pp.chmod(0o111)
        self.assertFalse(os.access(pp, os.R_OK), "precondition: pre-push unreadable")
        self.assertUndetermined(fx.check(fx.main))

    def test_unreadable_scripts_dir_is_undetermined(self):
        fx = Fx(self)
        fx.write_hook(fx.main, "pre-commit", hook(["run check-a.py a"]))
        fx.add_scanner(fx.main, "check-a.py")
        fx.set_hooks_path("absolute")
        sd = fx.main / "scripts"
        sd.chmod(0o000)
        self.assertFalse(os.access(sd, os.R_OK), "precondition: scripts dir unreadable")
        self.assertUndetermined(fx.check(fx.main))

    def test_missing_scripts_dir_is_undetermined(self):
        fx = Fx(self)
        fx.write_hook(fx.main, "pre-commit", hook(["run check-a.py a"]))
        shutil.rmtree(fx.main / "scripts")
        fx.set_hooks_path("absolute")
        self.assertUndetermined(fx.check(fx.main))

    def test_zero_recognisable_invocations_is_undetermined(self):
        fx = Fx(self)
        fx.write_hook(fx.main, "pre-commit", "#!/bin/sh\n# run check-a.py a\necho hi\nexit 0\n")
        fx.add_scanner(fx.main, "check-a.py")
        fx.set_hooks_path("absolute")
        p = fx.check(fx.main)
        self.assertUndetermined(p)

    def _assert_unparseable(self, extra):
        fx = Fx(self)
        fx.write_hook(fx.main, "pre-commit", hook(["run check-a.py a"] + extra))
        fx.add_scanner(fx.main, "check-a.py")
        fx.set_hooks_path("absolute")
        p = fx.check(fx.main)
        self.assertEqual(p.returncode, 2, out(p))
        self.assertIn("parse", p.stderr, out(p))

    def test_unbalanced_double_quote_is_undetermined(self):
        self._assert_unparseable(['echo "never closed'])

    def test_unbalanced_single_quote_is_undetermined(self):
        self._assert_unparseable(["echo 'never closed"])

    def test_unbalanced_command_substitution_is_undetermined(self):
        self._assert_unparseable(['x=$(python3 scripts/check-a.py'])

    def test_unterminated_heredoc_is_undetermined(self):
        self._assert_unparseable(["cat <<'EOF'", "python3 scripts/check-a.py"])

    def test_zero_scanners_in_scripts_is_undetermined(self):
        fx = Fx(self)
        fx.write_hook(fx.main, "pre-commit", hook(["run check-a.py a"]))
        fx.set_hooks_path("absolute")
        self.assertUndetermined(fx.check(fx.main))

    def test_unreadable_hookspath_setting_is_undetermined(self):
        fx = Fx(self)
        fx.write_hook(fx.main, "pre-commit", hook(["run check-a.py a"]))
        fx.add_scanner(fx.main, "check-a.py")
        fx.set_hooks_path("absolute")
        cfg = fx.main / ".git" / "config"
        cfg.write_text(cfg.read_text() + "\n[broken\n")
        self.assertUndetermined(fx.check(fx.main))

    def test_not_a_git_repo_is_undetermined(self):
        fx = Fx(self)
        d = fx.root / "plain"
        d.mkdir()
        self.assertUndetermined(fx.check(d))


class RealHooksSyntax(unittest.TestCase):
    """The parser must recognise the invocation forms the real hooks use."""

    def test_real_hooks_invocations_are_recognised(self):
        fx = Fx(self)
        for name in os.listdir(SRC / ".githooks"):
            src = SRC / ".githooks" / name
            if src.is_file():
                shutil.copy2(src, fx.main / ".githooks" / name)
                (fx.main / ".githooks" / name).chmod(0o755)
        expected = [
            "check-prompt-injection.py",  # run helper, pre-commit
            "check-clippy-lints.py",  # run helper, pre-commit
            "check-test-weakening.py",  # SCANNER= variable, commit-msg
            "check-plugin-rollout.py",  # $(python3 scripts/...), pre-push
            "check-compile-fail.py",  # subshell python3 scripts/..., pre-push
        ]
        for n in expected:
            fx.add_scanner(fx.main, n)
        fx.set_hooks_path("absolute")
        p = fx.check(fx.main)
        self.assertEqual(p.returncode, 0, out(p))


if __name__ == "__main__":
    unittest.main()
