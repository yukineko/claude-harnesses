#!/usr/bin/env python3
"""ae4543d5: guard-maintree-bash.py must judge a write into the main tree by its
EFFECT, not by argv[0] / a single shell spelling.

Each REFUSE case is a write that lands in the main tree through a spelling the
guard used to wave through (interpreter wrappers, `~`/`$HOME`/`$PWD`, a `cd` /
`git -C` re-anchor before an absolute main path, in-place flag bundles,
downloaders and extractors).

Each ALLOW case is the same effect aimed at a linked worktree or a scratch dir
outside main, or a pure READ of main. They exist so a guard that simply refuses
everything cannot pass this file (anti-vacuity).

    python3 -m unittest scripts.test_guard_maintree_bash_ae4543d5
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
GUARD = SCRIPTS / "guard-maintree-bash.py"


def _env(**extra):
    env = dict(os.environ)
    env.update(
        GIT_CONFIG_GLOBAL=os.devnull,
        GIT_CONFIG_NOSYSTEM="1",
        GIT_AUTHOR_NAME="t",
        GIT_AUTHOR_EMAIL="t@example.invalid",
        GIT_COMMITTER_NAME="t",
        GIT_COMMITTER_EMAIL="t@example.invalid",
    )
    env.update(extra)
    return env


def _git(cwd, *args):
    r = subprocess.run(["git", *args], cwd=cwd, env=_env(), capture_output=True, text=True)
    if r.returncode != 0:
        raise AssertionError(f"fixture void: git {args}: {r.stderr}")


class Fixture:
    def __init__(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="ae45-")).resolve()
        self.home = self.tmp  # HOME, so ~/main is the main tree
        self.main = self.tmp / "main"
        self.wt = self.tmp / "wt"
        self.out = self.tmp / "scratch"
        self.main.mkdir()
        self.out.mkdir()
        _git(self.main, "init", "-q", "-b", "main")
        (self.main / "f.txt").write_text("a\n")
        _git(self.main, "add", "f.txt")
        _git(self.main, "commit", "-q", "-m", "seed")
        _git(self.main, "worktree", "add", "-q", "-b", "feat", str(self.wt))

    def close(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def run(self, command, cwd=None):
        payload = {"tool_name": "Bash", "tool_input": {"command": command}}
        env = _env(CLAUDE_PROJECT_DIR=str(self.main), HOME=str(self.home))
        env.pop("PWD", None)
        r = subprocess.run(
            [sys.executable, str(GUARD)],
            input=json.dumps(payload), capture_output=True, text=True,
            cwd=str(cwd or self.main), env=env,
        )
        return r.returncode, r.stderr


# `{T}` is replaced by the directory under test: main (must refuse) or the
# worktree / scratch dir (must allow). Same command text, different target.
SAME_EFFECT = [
    "echo hi > {T}/p.txt",
    "sh -c 'echo hi > {T}/p.txt'",
    "bash -lc 'echo hi > {T}/p.txt'",
    "bash -ec 'echo hi > {T}/p.txt'",
    "eval 'echo hi > {T}/p.txt'",
    "env FOO=1 sh -c 'echo hi > {T}/p.txt'",
    "nohup bash -c 'rm {T}/f.txt'",
    "python3 -c \"open('{T}/p.txt','w').write('x')\"",
    "python3 -c \"import pathlib; pathlib.Path('{T}/p.txt').write_text('x')\"",
    "python3 -c \"import os; os.system('echo hi > {T}/p.txt')\"",
    "python3 - <<'EOF'\nopen('{T}/p.txt', 'w').write('it''s')\nEOF",
    "awk 'BEGIN {{ print \"x\" > \"{T}/p.txt\" }}'",
    "node -e \"require('fs').writeFileSync('{T}/p.txt','x')\"",
    "perl -e 'open(my $f, \">\", \"{T}/p.txt\")'",
    "ruby -e 'File.write(\"{T}/p.txt\", \"x\")'",
    "sed --in-place 's/a/b/' {T}/f.txt",
    "sed --in-place=.bak 's/a/b/' {T}/f.txt",
    "sed -i.bak 's/a/b/' {T}/f.txt",
    "sed -Ei 's/a/b/' {T}/f.txt",
    "sed -i -e 's/a/b/' {T}/f.txt",
    "perl -pi -e 's/a/b/' {T}/f.txt",
    "perl -i.bak -pe 's/a/b/' {T}/f.txt",
    "ruby -pi -e 'gsub(/a/, \"b\")' {T}/f.txt",
    "curl -o {T}/p.txt https://example.invalid/x",
    "curl -sSLo {T}/p.txt https://example.invalid/x",
    "curl --output={T}/p.txt https://example.invalid/x",
    "wget -O {T}/p.txt https://example.invalid/x",
    "wget --output-document={T}/p.txt https://example.invalid/x",
    "tar -xf /nonexistent/a.tar -C {T}",
    "tar --directory={T} -xzf /nonexistent/a.tgz",
    "tar xzf /nonexistent/a.tgz -C {T}",
    "unzip /nonexistent/a.zip -d {T}",
    "find {T} -name '*.txt' -delete",
    "find {T} -name '*.txt' -exec rm {{}} \\;",
    "cd /tmp && echo x > {T}/p.txt",
    "git -C /tmp status; echo x > {T}/p.txt",
    "(cd /tmp && echo x > {T}/p.txt)",
]


class SameEffectIntoMainIsRefused(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.f = Fixture()

    @classmethod
    def tearDownClass(cls):
        cls.f.close()

    def test_into_main_refused(self):
        for tmpl in SAME_EFFECT:
            cmd = tmpl.format(T=self.f.main)
            with self.subTest(cmd=cmd):
                rc, err = self.f.run(cmd)
                self.assertEqual(rc, 2, f"write into main was allowed: {cmd!r}")

    def test_into_worktree_allowed(self):
        for tmpl in SAME_EFFECT:
            cmd = tmpl.format(T=self.f.wt)
            with self.subTest(cmd=cmd):
                rc, err = self.f.run(cmd)
                self.assertEqual(rc, 0, f"worktree write refused: {cmd!r}\n{err[:300]}")

    def test_into_scratch_allowed(self):
        for tmpl in SAME_EFFECT:
            cmd = tmpl.format(T=self.f.out)
            with self.subTest(cmd=cmd):
                rc, err = self.f.run(cmd)
                self.assertEqual(rc, 0, f"scratch write refused: {cmd!r}\n{err[:300]}")

    # --- deterministic expansions --------------------------------------
    def test_home_tilde_pwd_into_main_refused(self):
        for cmd in (
            "echo x > ~/main/p.txt",
            "echo x > $HOME/main/p.txt",
            "echo x > ${HOME}/main/p.txt",
            "echo x > \"$HOME\"/main/p.txt",
            "echo x > $PWD/p.txt",          # hook cwd is main
            "sh -c 'echo x > ~/main/p.txt'",
            "python3 -c \"import os; open(os.path.expanduser('~/main/p.txt'),'w')\"",
        ):
            with self.subTest(cmd=cmd):
                self.assertEqual(self.f.run(cmd)[0], 2, cmd)

    def test_home_tilde_pwd_outside_main_allowed(self):
        for cmd in (
            "echo x > ~/scratch/p.txt",
            "echo x > $HOME/wt/p.txt",
            f"echo x > $PWD/p.txt",
        ):
            cwd = self.f.wt if "$PWD" in cmd else None
            with self.subTest(cmd=cmd):
                rc, err = self.f.run(cmd, cwd=cwd)
                self.assertEqual(rc, 0, f"{cmd}: {err[:300]}")

    def test_path_rebuilt_from_home_fragment_refused(self):
        # `$HOME + '/main/p.txt'` never spells main literally; the tail fragment
        # does line up with the main root's last component.
        cmd = "python3 -c \"import os; open(os.environ['HOME'] + '/main/p.txt', 'w')\""
        self.assertEqual(self.f.run(cmd)[0], 2)

    def test_interpreter_write_without_any_literal_path_refused(self):
        cmd = "python3 -c \"import sys; open(sys.argv[1] + sys.argv[2], 'w')\""
        self.assertEqual(self.f.run(cmd)[0], 2)

    def test_eval_of_unknown_text_refused(self):
        self.assertEqual(self.f.run('eval "$CMD"')[0], 2)
        self.assertEqual(self.f.run("sh -c \"$CMD\"")[0], 2)

    # --- variables assigned earlier in the same command -------------------
    def test_same_command_variable_outside_main_allowed(self):
        for d in (self.f.out, self.f.wt, "/tmp"):
            for cmd in (
                f"S={d}; echo x > $S/f",
                f"S={d}\necho x > \"$S\"/f",
                f"export S={d}; echo x > ${{S}}/f",
                f"S={d} && mkdir -p $S/sub && cp /etc/hosts $S/sub/h",
                f"S={d}; python3 -c \"open('$S/p.txt','w')\"",
            ):
                with self.subTest(cmd=cmd):
                    rc, err = self.f.run(cmd)
                    self.assertEqual(rc, 0, f"{cmd}: {err[:300]}")

    def test_same_command_variable_into_main_refused(self):
        m = self.f.main
        for cmd in (
            f"S={m}; echo x > $S/f",
            f"export S={m}; echo x > ${{S}}/f",
            f"S={self.f.out}; S={m}; echo x > $S/f",            # last assignment wins
            f"S={m}; python3 -c \"open('$S/p.txt','w')\"",
            f"S={m}; sh -c \"echo x > $S/f\"",
        ):
            with self.subTest(cmd=cmd):
                self.assertEqual(self.f.run(cmd)[0], 2, cmd)

    def test_untrackable_variable_refused(self):
        o = self.f.out
        for cmd in (
            "S=$(mktemp -d); echo x > $S/f",           # value only known at runtime
            f"[ -d /nope ] && S={o}; echo x > $S/f",   # assignment may not run
            f"(S={o}); echo x > $S/f",                 # subshell does not leak
            f"S={o} true; echo x > $S/f",              # prefix assignment: env only
            f"S={o} | true; echo x > $S/f",            # pipeline stage: subshell
            "echo x > $INHERITED/f",                   # never assigned here
            f"if true; then S={o}; fi; echo x > $S/f",  # branch may not run
        ):
            with self.subTest(cmd=cmd):
                self.assertEqual(self.f.run(cmd)[0], 2, cmd)

    def test_inherited_variable_under_disjoint_literal_prefix_allowed(self):
        rc, err = self.f.run(f"echo x > {self.f.out}/$INHERITED/f")
        self.assertEqual(rc, 0, err[:300])

    # --- re-anchoring ---------------------------------------------------
    def test_relative_after_cd_outside_allowed(self):
        for cmd in (
            f"cd {self.f.wt} && echo x > rel.txt",
            f"cd {self.f.wt} && sed -i 's/a/b/' f.txt",
            f"cd {self.f.wt} && python3 -c \"open('o.txt','w').write('x')\"",
            "cd /tmp && echo x > rel.txt",
        ):
            with self.subTest(cmd=cmd):
                rc, err = self.f.run(cmd)
                self.assertEqual(rc, 0, f"{cmd}: {err[:300]}")

    def test_relative_after_uncertain_cd_refused(self):
        for cmd in (
            "(cd /tmp); echo x > rel.txt",       # subshell cd does not persist
            "cd /no/such/dir; echo x > rel.txt",  # failed cd leaves cwd on main
            "cd /tmp || echo x > rel.txt",       # runs only if cd failed
            "cd /tmp | true; echo x > rel.txt",  # pipeline cd is a subshell
            "cd \"$X\" && echo x > rel.txt",      # unknown destination
            "git -C /tmp status; echo x > rel.txt",
        ):
            with self.subTest(cmd=cmd):
                self.assertEqual(self.f.run(cmd)[0], 2, cmd)

    # --- reads of main stay allowed --------------------------------------
    def test_reads_of_main_allowed(self):
        m = self.f.main
        for cmd in (
            f"cat {m}/f.txt",
            f"python3 -c \"print(open('{m}/f.txt').read())\"",
            f"node -e \"console.log(require('fs').readFileSync('{m}/f.txt','utf8'))\"",
            f"awk '$1 > 3 {{ print $1 }}' {m}/f.txt",
            f"awk '{{ print $1 }}' {m}/f.txt",
            f"perl -ne 'print' {m}/f.txt",
            f"ruby -ne 'puts $_' {m}/f.txt",
            f"sed -n 's/a/b/p' {m}/f.txt",
            f"bash -lc 'cat {m}/f.txt'",
            "curl -s https://example.invalid/x",
            "wget -qO- https://example.invalid/x",
            f"tar -tf {m}/a.tar",
            f"tar -xf {m}/a.tar -C {self.f.wt}",
            f"unzip -l {m}/a.zip",
            f"unzip {m}/a.zip -d {self.f.wt}",
            f"find {m} -name '*.txt'",
            f"echo x 2>&1 > {self.f.wt}/x.txt",
            f"git -C {m} log --oneline | head",
        ):
            with self.subTest(cmd=cmd):
                rc, err = self.f.run(cmd)
                self.assertEqual(rc, 0, f"{cmd}: {err[:300]}")


if __name__ == "__main__":
    unittest.main()
