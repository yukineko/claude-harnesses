#!/usr/bin/env python3
"""b5358f58: guard-maintree-bash.py judges only the DESTINATION of cp / ln /
install (and of mv, plus mv's source, which mv removes) as a write target.

Observed 2026-09-08 at df570513 and re-measured at 4a7fcb1e: `cp <main>/f
<worktree>/f.copy` was refused although only the worktree is written. CLAUDE.md
section 8 tells sessions to bring a file from main into a worktree exactly
this way.

ALLOW cases read main and write only a worktree / scratch dir. REFUSE cases
write main (as the destination, as mv's removed source, as an rsync
--remove-source-files source, or through a hard link that shares main's
inode). The REFUSE cases are the anti-vacuity control: a guard that allows
every cp cannot pass this file.

    python3 -m unittest scripts.test_guard_maintree_bash_b5358f58
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
        self.tmp = Path(tempfile.mkdtemp(prefix="b535-")).resolve()
        self.home = self.tmp / "home"
        self.main = self.tmp / "main"
        self.wt = self.tmp / "wt"
        self.out = self.tmp / "scratch"
        for d in (self.home, self.main, self.out):
            d.mkdir()
        _git(self.main, "init", "-q", "-b", "main")
        (self.main / "f.txt").write_text("a\n")
        (self.main / "g.txt").write_text("b\n")
        _git(self.main, "add", "f.txt", "g.txt")
        _git(self.main, "commit", "-q", "-m", "seed")
        _git(self.main, "worktree", "add", "-q", "-b", "feat", str(self.wt))

    def close(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def run(self, command, cwd):
        payload = {"tool_name": "Bash", "tool_input": {"command": command},
                   "cwd": str(cwd), "session_id": "test-b5358f58"}
        env = _env(CLAUDE_PROJECT_DIR=str(self.main), HOME=str(self.home))
        env.pop("PWD", None)
        r = subprocess.run(
            [sys.executable, str(GUARD)],
            input=json.dumps(payload), capture_output=True, text=True,
            cwd=str(cwd), env=env,
        )
        return r.returncode, r.stderr


class DestinationOnly(unittest.TestCase):
    def setUp(self):
        self.f = Fixture()
        self.M, self.W, self.O = str(self.f.main), str(self.f.wt), str(self.f.out)

    def tearDown(self):
        self.f.close()

    def _expect(self, cmds, rc_want, cwds=None):
        for cwd in cwds or (self.f.main, self.f.wt):
            for c in cmds:
                with self.subTest(cmd=c, cwd=str(cwd)):
                    rc, err = self.f.run(c, cwd)
                    self.assertEqual(rc, rc_want, f"{c!r} from {cwd}: {err[:300]}")

    # --- reads of main written elsewhere: ALLOW ----------------------------
    def test_cp_from_main_into_worktree_or_scratch_is_allowed(self):
        M, W, O = self.M, self.W, self.O
        self._expect([
            f"cp {M}/f.txt {W}/f.copy",
            f"cp {M}/f.txt {O}/f.copy",
            f"cp {M}/f.txt {M}/g.txt {W}/",
            f"cp -r {M} {O}/snap",
            f"cp -p {M}/f.txt {W}/f.copy",
            f"cp -- {M}/f.txt {W}/f.copy",
            f"gcp {M}/f.txt {W}/f.copy",
            f"cp {M}/.githooks/pre-commit {O}/pc",
            f"find {M} -name f.txt -exec cp {{}} {W}/ \\;",
            f"cd {W} && cp {M}/f.txt .",
        ], 0)

    def test_cp_relative_source_in_main_cwd_into_worktree_is_allowed(self):
        self._expect([f"cp f.txt {self.W}/f.copy"], 0, cwds=(self.f.main,))

    def test_target_directory_spellings_name_the_destination(self):
        M, W = self.M, self.W
        cmds = []
        for prog in ("cp", "gcp", "install", "ginstall", "mv"):
            src = f"{W}/f.txt"
            cmds += [
                f"{prog} -t {M} {src}",
                f"{prog} -t{M} {src}",
                f"{prog} -vt {M} {src}",
                f"{prog} -vt{M} {src}",
                f"{prog} --target-directory={M} {src}",
                f"{prog} --target-directory {M} {src}",
                f"{prog} --target {M} {src}",
                f"{prog} --targ={M} {src}",
            ]
        self._expect(cmds, 2)
        self._expect([
            f"cp -t {W} {M}/f.txt",
            f"cp -t{W} {M}/f.txt {M}/g.txt",
            f"cp -vt {W} {M}/f.txt",
            f"cp --target-directory={W} {M}/f.txt",
            f"cp --target-directory {W} {M}/f.txt",
            f"cp {M}/f.txt --target-directory {W}",
            f"gcp -t {W} {M}/f.txt",
            f"install -t {W} {M}/f.txt",
            f"ginstall --target-directory={W} {M}/f.txt",
            f"ln -s -t {W} {M}/f.txt",
            f"gln -st {W} {M}/f.txt",
        ], 0)

    def test_install_follows_the_cp_rule(self):
        M, W, O = self.M, self.W, self.O
        self._expect([
            f"install {M}/f.txt {W}/f.copy",
            f"install -m 0644 {M}/f.txt {O}/f.copy",
            f"ginstall -m 644 {M}/f.txt {W}/f.copy",
            f"install -d {W}/newdir {O}/other",
        ], 0)
        self._expect([
            f"install {W}/f.txt {M}/new.txt",
            f"install -m 0644 {W}/f.txt {M}/new.txt",
            f"ginstall {W}/f.txt {M}/",
            f"install -d {M}/newdir",
            f"install -d {W}/a {M}/b",
            f"install -m 755 -d {M}/newdir",
        ], 2)

    def test_symlink_in_worktree_pointing_at_main_is_allowed(self):
        M, W = self.M, self.W
        self._expect([
            f"ln -s {M}/f.txt {W}/link",
            f"ln -sf {M}/f.txt {W}/link",
            f"gln -s {M}/f.txt {W}/link",
            f"ln -s {M}/f.txt {M}/g.txt {W}/",
        ], 0)
        self._expect([f"ln -s {M}/f.txt"], 0, cwds=(self.f.wt,))

    # --- writes into main: REFUSE (anti-vacuity) ----------------------------
    def test_destination_in_main_is_refused(self):
        M, W, O = self.M, self.W, self.O
        self._expect([
            f"cp {W}/f.txt {M}/g.txt",
            f"cp {O}/x {M}/",
            f"cp {M}/f.txt {M}/g.txt",
            f"cp -r {W} {M}/sub",
            f"cp -- {W}/f.txt {M}/g.txt",
            f"cp {W}/f.txt {M}/g.txt -S .bak",
            f"cp {W}/f.txt {M}/g.txt --sparse always",
            f"cp -Stmp {W}/f.txt {M}/g.txt",
            f"gcp {W}/f.txt {M}/g.txt",
            f"cd {M} && cp {W}/f.txt .",
        ], 2)
        self._expect([f"cp {W}/f.txt g.txt"], 2, cwds=(self.f.main,))

    def test_mv_counts_its_source_as_a_write(self):
        M, W, O = self.M, self.W, self.O
        self._expect([
            f"mv {M}/f.txt {W}/f.txt",
            f"mv {M}/f.txt {O}/",
            f"mv -t {W} {M}/f.txt",
            f"mv --target-directory={W} {M}/f.txt",
            f"gmv {M}/f.txt {W}/f.txt",
            f"mv {W}/f.txt {M}/g.txt",
            f"find {M} -name f.txt -exec mv {{}} {W}/ \\;",
        ], 2)
        self._expect([f"mv f.txt {W}/f.txt"], 2, cwds=(self.f.main,))
        self._expect([f"mv {W}/f.txt {O}/f.txt"], 0)

    def test_rsync_remove_source_files_counts_its_source_as_a_write(self):
        M, O = self.M, self.O
        self._expect([f"rsync -a {M}/f.txt {O}/"], 0)
        self._expect([
            f"rsync -a --remove-source-files {M}/f.txt {O}/",
            f"rsync -a {M}/f.txt {O}/ --remove-source-files",
            f"rsync --remove-sent-files {M}/f.txt {O}/",
        ], 2)

    def test_link_path_in_main_is_refused(self):
        M, W = self.M, self.W
        self._expect([
            f"ln -s {W}/f.txt {M}/link",
            f"ln -s -t {M} {W}/f.txt",
            f"ln -s --target-directory={M} {W}/f.txt",
            f"gln -s {W}/f.txt {M}/link",
        ], 2)
        # one operand: the link is created in the cwd
        self._expect([f"ln -s {W}/f.txt"], 2, cwds=(self.f.main,))

    def test_hard_link_to_a_main_file_is_refused(self):
        # A hard link shares main's inode: a later write through the worktree
        # path changes main's file, and no path this guard resolves shows it.
        M, W = self.M, self.W
        self._expect([
            f"ln {M}/f.txt {W}/hard",
            f"ln -f {M}/f.txt {W}/hard",
            f"gln -t {W} {M}/f.txt",
            f"cp -l {M}/f.txt {W}/hard",
            f"cp -al {M}/f.txt {W}/hard",
            f"cp --link {M}/f.txt {W}/hard",
        ], 2)

    def test_undetermined_destination_judges_every_operand(self):
        # An unknown or ambiguous GNU long option could take any following
        # word as its value, so the destination cannot be placed: every
        # operand is judged, and a main source is then refused.
        M, W = self.M, self.W
        self._expect([
            f"cp --frobnicate {M}/f.txt {W}/x",
            f"cp --s {M}/f.txt {W}/x",
            f"cp {M}/f.txt {W}/x -t",
        ], 2)
        # BSD reading: every word after the first operand is an operand, so
        # the destination of `cp a b -v` is `-v` (here: inside main's cwd).
        self._expect([f"cp {W}/f.txt {W}/g.txt -v"], 2, cwds=(self.f.main,))
        self._expect([f"cp {W}/f.txt {W}/g.txt -v"], 0, cwds=(self.f.wt,))

    def test_write_through_an_existing_symlink_into_main_is_refused(self):
        # The symlink itself is allowed (above); a LATER write through it is
        # judged at its resolved path, which is main.
        (self.f.wt / "link").symlink_to(self.f.main / "f.txt")
        self._expect([
            f"echo x > {self.W}/link",
            f"cp {self.O}/x {self.W}/link",
        ], 2)

    def test_hook_machinery_destination_still_refused_from_a_worktree(self):
        self._expect([
            f"cp {self.O}/x .git/hooks/pre-commit",
            f"cp {self.O}/x {self.M}/.githooks/pre-commit",
        ], 2, cwds=(self.f.main,))


if __name__ == "__main__":
    unittest.main()
