#!/usr/bin/env python3
"""Independent verifier tests for ae4543d5 (guard-maintree-bash.py judged by effect).

Written by the condukt verifier, not by the implementing worker (CLAUDE.md 2(a)).

Every command is judged with the hook process cwd set to a LINKED WORKTREE and
CLAUDE_PROJECT_DIR set to the main checkout, which is the realistic shape of a
session that started in main and then moved into a worktree (CLAUDE.md 8).

  * MustRefuse: a write that lands in the main tree, spelled through the
    routes named in the done_criteria (interpreter wrappers, ~/$HOME/${HOME},
    re-anchors, in-place bundles) plus spellings found by the verifier's own
    bypass hunt.
  * MustAllow: commands that do not write into main. Some of them are
    over-blocks the verifier found; those are expected RED until fixed.

    python3 -m unittest scripts/test_guard_maintree_bash_ae4543d5_verify.py
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
Q = "'"


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


class Fx:
    def __init__(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="ae45v-")).resolve()
        self.main = self.tmp / "main"
        self.wt = self.tmp / "wt"
        self.scratch = self.tmp / "scratch"
        self.main.mkdir()
        self.scratch.mkdir()
        _git(self.main, "init", "-q", "-b", "main")
        (self.main / "f.txt").write_text("a\n")
        _git(self.main, "add", "f.txt")
        _git(self.main, "commit", "-q", "-m", "seed")
        _git(self.main, "worktree", "add", "-q", "-b", "feat", str(self.wt))
        (self.wt / "a.txt").write_text("a\n")

    def close(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def run(self, command, cwd=None):
        payload = {"tool_name": "Bash", "tool_input": {"command": command}}
        env = _env(CLAUDE_PROJECT_DIR=str(self.main), HOME=str(self.tmp))
        env.pop("PWD", None)
        r = subprocess.run(
            [sys.executable, str(GUARD)],
            input=json.dumps(payload), capture_output=True, text=True,
            cwd=str(cwd or self.wt), env=env,
        )
        return r.returncode, r.stderr


class _Base(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.f = Fx()

    @classmethod
    def tearDownClass(cls):
        cls.f.close()

    def refuse(self, cmds):
        for c in cmds:
            with self.subTest(cmd=c):
                rc, err = self.f.run(c)
                self.assertEqual(rc, 2, f"write into main ALLOWED: {c!r}")

    def allow(self, cmds):
        for c in cmds:
            with self.subTest(cmd=c):
                rc, err = self.f.run(c)
                self.assertEqual(rc, 0, f"non-main command REFUSED: {c!r}\n{err[:300]}")


class MustRefuseDoneCriteria(_Base):
    """The done_criteria routes, independently re-spelled."""

    def test_interpreter_wrappers(self):
        M = self.f.main
        self.refuse([
            f"sh -xc {Q}rm {M}/f.txt{Q}",
            f"bash -o pipefail -c {Q}echo x > {M}/p.txt{Q}",
            f"zsh -c {Q}rm {M}/f.txt{Q}",
            f"bash <<{Q}EOF{Q}\nrm {M}/f.txt\nEOF",
            f"eval rm {M}/f.txt",
            f"X={M}; eval \"rm $X/f.txt\"",
            f"python3 -c \"import os; os.remove({Q}{M}/f.txt{Q})\"",
            f"python3 <<{Q}EOF{Q}\nimport shutil\nshutil.copy({Q}/etc/hosts{Q}, {Q}{M}/p.txt{Q})\nEOF",
            f"node --eval \"require({Q}fs{Q}).rmSync({Q}{M}/f.txt{Q})\"",
            f"awk {Q}BEGIN{{system(\"rm {M}/f.txt\")}}{Q}",
            f"awk {Q}{{print > \"{M}/p.txt\"}}{Q} /etc/hosts",
            f"x=$(rm {M}/f.txt)",
            f"echo \"$(rm {M}/f.txt)\"",
            f"if true; then rm {M}/f.txt; fi",
        ])

    def test_home_tilde_expansion(self):
        self.refuse([
            "echo x > ~/main/p.txt",
            "echo x > ${HOME}/main/p.txt",
            "echo x > \"${HOME}\"/main/p.txt",
            "echo x > \"$HOME/main/p.txt\"",
            "ln -sf /etc/hosts ~/main/p.txt",
            "cd ~/main && echo x > p.txt",
            "cd $HOME/main; echo x > p.txt",
        ])

    def test_unexpandable_that_could_reach_main(self):
        self.refuse([
            "echo x > $UNKNOWN_VAR/p.txt",
            f"echo x > {self.f.tmp}/$UNKNOWN_VAR/p.txt",
            "echo x > ~nobody/p.txt",
        ])

    def test_reanchor_then_absolute_main(self):
        M = self.f.main
        self.refuse([
            f"cd {self.f.wt} && rm {M}/f.txt",
            f"git -C {self.f.wt} status && rm {M}/f.txt",
            f"pushd {M} && echo x > p.txt",
            f"env -C {M} touch p.txt",
            f"command rm {M}/f.txt",
            f"\\rm {M}/f.txt",
            f"/bin/rm {M}/f.txt",
        ])

    def test_inplace_bundles(self):
        M = self.f.main
        self.refuse([
            f"perl -0pi -e {Q}s/a/b/{Q} {M}/f.txt",
            f"perl -wpi -e {Q}s/a/b/{Q} {M}/f.txt",
            f"perl -p -i -e {Q}s/a/b/{Q} {M}/f.txt",
            f"ruby -i -pe {Q}x{Q} {M}/f.txt",
            f"sed --in-place --expression={Q}s/a/b/{Q} {M}/f.txt",
            f"sed -e {Q}s/a/b/{Q} -i {M}/f.txt",
            f"sed -n -i {Q}s/a/b/p{Q} {M}/f.txt",
            f"gawk -i inplace {Q}{{print}}{Q} {M}/f.txt",
        ])


class MustRefuseBypassHunt(_Base):
    """Spellings the verifier found still ALLOWED. Expected RED until fixed."""

    def test_target_directory_option(self):
        M = self.f.main
        self.refuse([
            f"cp --target-directory={M} /etc/hosts",
            f"mv --target-directory={M} {self.f.scratch}/nope",
            f"install --target-directory={M} /etc/hosts",
            f"cp -t{M} /etc/hosts",
        ])

    def test_python_import_aliases(self):
        M = self.f.main
        self.refuse([
            f"python3 -c \"from os import remove; remove({Q}{M}/f.txt{Q})\"",
            f"python3 -c \"import os as o; o.remove({Q}{M}/f.txt{Q})\"",
            f"python3 -c \"import shutil as s; s.rmtree({Q}{M}{Q})\"",
            f"python3 -c \"__import__({Q}os{Q}).remove({Q}{M}/f.txt{Q})\"",
        ])

    def test_node_aliases(self):
        M = self.f.main
        self.refuse([
            f"node -e \"const {{writeFileSync: w}} = require({Q}fs{Q}); w({Q}{M}/p.txt{Q},{Q}x{Q})\"",
            f"node -e \"require({Q}fs{Q})[{Q}writeFileSync{Q}]({Q}{M}/p.txt{Q},{Q}x{Q})\"",
        ])

    @unittest.skipUnless(
        os.path.exists(str(Path(tempfile.gettempdir()).resolve()).upper()),
        "filesystem is case-sensitive",
    )
    def test_case_variant_of_main_path_on_case_insensitive_fs(self):
        M = str(self.f.main)
        variant = M[:-4] + "MAIN"
        self.assertTrue(os.path.isdir(variant), "fixture: case-insensitive alias missing")
        self.refuse([f"echo x > {variant}/p.txt", f"rm {variant}/f.txt"])


class MustAllow(_Base):
    def test_worktree_and_scratch_targets(self):
        W, S = self.f.wt, self.f.scratch
        self.allow([
            f"sh -c {Q}echo x > {W}/p.txt{Q}",
            f"python3 -c \"import os; os.remove({Q}{W}/a.txt{Q})\"",
            f"perl -pi -e {Q}s/a/b/{Q} {W}/a.txt",
            f"sed --in-place {Q}s/a/b/{Q} {W}/a.txt",
            f"ruby -pi -e {Q}x{Q} {W}/a.txt",
            f"cd {W} && echo x > p.txt",
            f"cd {W} && python3 -c \"open({Q}out.json{Q},{Q}w{Q})\"",
            f"D={S}; echo x > $D/a.txt",
            f"D={S}; python3 -c \"open({Q}$D/a.json{Q},{Q}w{Q})\"",
            "echo x > ~/scratch/a.txt",
            f"cat {self.f.main}/f.txt > {S}/copy.txt",
            "cargo build",
            "make",
            ". \"$HOME/.cargo/env\" && cargo test -p x",
            "bash -lc \"cargo test\"",
            f"echo x > $PWD/p.txt",
        ])

    def test_eval_of_env_setup_is_not_refused(self):
        # `eval "$(brew shellenv)"` / `eval "$(ssh-agent -s)"` only set
        # environment variables; base allowed them. Over-block found by the
        # verifier (refused even with cwd outside main). Expected RED.
        self.allow([
            "eval \"$(brew shellenv)\"",
            "eval \"$(ssh-agent -s)\"",
        ])

    def test_awk_pipe_char_inside_string_is_not_a_write(self):
        # A `|` inside an awk string literal is output text, not a pipe.
        # Over-block found by the verifier. Expected RED.
        self.allow([
            f"awk -F{Q}|{Q} {Q}{{print $1 \"|\" $2}}{Q} {self.f.wt}/a.txt",
            f"awk {Q}{{print $1 \" | \" $2}}{Q} {self.f.wt}/a.txt",
        ])

    def test_loop_over_worktree_absolute_glob(self):
        # Every value of $f is under the worktree; the literal glob prefix
        # proves it cannot reach main.
        W = self.f.wt
        self.allow([f"for f in {W}/*.txt; do sed -i {Q}{Q} s/a/b/ \"$f\"; done"])


if __name__ == "__main__":
    unittest.main()
