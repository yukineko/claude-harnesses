#!/usr/bin/env python3
"""Independent verification of b5358f58 (written by the verifier, not the
implementer): guard-maintree-bash.py judges cp / mv / ln / install by what
they WRITE, so copying a file out of main into a worktree or scratch dir is
allowed, while anything that writes main (destination in main, mv's removed
source, a hard link to a main file) stays refused.

Every probe runs in a fresh HOME, a fresh TMPDIR and a fresh session id, so
the deny ledger of one probe can never replay into the next one: an ALLOW
here is this guard's own judgement of that single command.

The guard under test defaults to the sibling guard-maintree-bash.py and can be
pointed elsewhere with B5358F58_VERIFY_GUARD=<path> (used to observe the
allow-side tests RED against the pre-change guard).

    python3 -m unittest scripts.test_guard_maintree_bash_b5358f58_verify
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
import uuid
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
GUARD = Path(os.environ.get("B5358F58_VERIFY_GUARD") or SCRIPTS / "guard-maintree-bash.py")

ALLOW, DENY = 0, 2


def _env(**extra):
    env = {k: v for k, v in os.environ.items()
           if not k.startswith(("GIT_", "CLAUDE_"))}
    env.pop("PWD", None)
    env.pop("POSIXLY_CORRECT", None)
    env.update(
        GIT_CONFIG_GLOBAL=os.devnull,
        GIT_CONFIG_NOSYSTEM="1",
        GIT_AUTHOR_NAME="v",
        GIT_AUTHOR_EMAIL="v@example.invalid",
        GIT_COMMITTER_NAME="v",
        GIT_COMMITTER_EMAIL="v@example.invalid",
    )
    env.update(extra)
    return env


def _git(cwd, *args):
    r = subprocess.run(["git", *args], cwd=cwd, env=_env(),
                       capture_output=True, text=True)
    if r.returncode != 0:
        raise AssertionError(f"fixture void: git {args}: {r.stderr}")


class Repo:
    """A throwaway main repo with one linked worktree and a scratch dir."""

    def __init__(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="b535v-")).resolve()
        self.main = self.tmp / "mainrepo"
        self.wt = self.tmp / "wtree"
        self.out = self.tmp / "scratch"
        self.main.mkdir()
        self.out.mkdir()
        _git(self.main, "init", "-q", "-b", "main")
        (self.main / "f.txt").write_text("a\n")
        (self.main / "g.txt").write_text("b\n")
        (self.main / "sub").mkdir()
        (self.main / "sub" / "h.txt").write_text("c\n")
        (self.main / ".gitignore").write_text("ignored/\n")
        _git(self.main, "add", "-A")
        _git(self.main, "commit", "-q", "-m", "seed")
        _git(self.main, "worktree", "add", "-q", "-b", "feat", str(self.wt))
        (self.out / "x").write_text("x\n")
        # A directory symlink inside the worktree that points INTO main.
        (self.wt / "tomain").symlink_to(self.main / "sub")
        (self.wt / ".git_info_exclude_marker").write_text("")

    def close(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def run(self, command, cwd):
        probe = Path(tempfile.mkdtemp(prefix="b535v-probe-")).resolve()
        try:
            home, tmpd = probe / "home", probe / "tmp"
            home.mkdir()
            tmpd.mkdir()
            payload = {"tool_name": "Bash", "tool_input": {"command": command},
                       "cwd": str(cwd), "session_id": "b535v-" + uuid.uuid4().hex}
            env = _env(CLAUDE_PROJECT_DIR=str(self.main), HOME=str(home),
                       TMPDIR=str(tmpd))
            r = subprocess.run([sys.executable, str(GUARD)],
                               input=json.dumps(payload), capture_output=True,
                               text=True, cwd=str(cwd), env=env, timeout=60)
            return r.returncode, r.stderr
        finally:
            shutil.rmtree(probe, ignore_errors=True)


class Base(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.r = Repo()
        cls.M, cls.W, cls.O = str(cls.r.main), str(cls.r.wt), str(cls.r.out)

    @classmethod
    def tearDownClass(cls):
        cls.r.close()

    def expect(self, want, cmds, cwds=None):
        for cwd in cwds or (self.r.main, self.r.wt):
            for c in cmds:
                with self.subTest(cmd=c, cwd=str(cwd)):
                    rc, err = self.r.run(c, cwd)
                    self.assertEqual(rc, want, f"{c!r} from {cwd}: rc={rc} {err[:400]}")


class AllowedCopiesOutOfMain(Base):
    def test_main_to_worktree_and_scratch_from_both_cwds(self):
        M, W, O = self.M, self.W, self.O
        self.expect(ALLOW, [
            f"cp {M}/f.txt {W}/f.txt",
            f"cp {M}/f.txt {O}/",
            f"cp -R {M}/sub {O}/subcopy",
            f"cp -a {M}/f.txt {M}/g.txt {W}/sub/",
            f"cp -v -- {M}/f.txt {W}/v.txt",
            f"cp '{M}/f.txt' \"{W}/q.txt\"",
            f"/bin/cp {M}/f.txt {W}/abs.txt",
            f"cp -t {W} {M}/f.txt {M}/g.txt",
            f"cp -t{O} {M}/f.txt",
            f"cp -pt {O} {M}/f.txt",
            f"cp --target-directory={W} {M}/f.txt",
            f"cp --target-dir {W} {M}/f.txt",
            f"cp --ta={W} {M}/f.txt",
            f"cp {M}/f.txt -t {W}",
            f"cp -t {W} -- {M}/f.txt",
            f"install -m 0644 {M}/f.txt {O}/inst.txt",
            f"install -t {W} {M}/f.txt",
            f"ln -s {M}/f.txt {W}/sym",
            f"cp {M}/.git/config {O}/cfg",
        ])

    def test_allowed_through_wrappers(self):
        M, W, O = self.M, self.W, self.O
        self.expect(ALLOW, [
            f"env cp {M}/f.txt {W}/e.txt",
            f"env LC_ALL=C cp {M}/f.txt {W}/e.txt",
            f"command cp {M}/f.txt {W}/c.txt",
            f"nohup cp {M}/f.txt {O}/n.txt",
            f"time cp {M}/f.txt {O}/t.txt",
            f"timeout 5 cp {M}/f.txt {O}/to.txt",
            f"bash -c 'cp {M}/f.txt {W}/b.txt'",
            f"sh -c \"cp -t {O} {M}/f.txt\"",
            f"D={W}; cp {M}/f.txt \"$D\"/var.txt",
            f"find {M}/sub -name '*.txt' -exec cp {{}} {O}/ \\;",
        ])

    def test_relative_source_from_main_cwd(self):
        self.expect(ALLOW, [
            f"cp f.txt {self.W}/rel.txt",
            f"cp -r sub {self.O}/relsub",
            f"cp -t {self.O} f.txt g.txt",
        ], cwds=(self.r.main,))


class RefusedWritesIntoMain(Base):
    def test_destination_in_main(self):
        M, W, O = self.M, self.W, self.O
        self.expect(DENY, [
            f"cp {W}/f.txt {M}/new.txt",
            f"cp {O}/x {M}/",
            f"cp {O}/x {M}/sub",
            f"cp -R {W}/sub {M}/sub2",
            f"cp -- {O}/x {M}/dd.txt",
            f"cp -fv {O}/x {M}/fv.txt",
            f"cp '{O}/x' \"{M}/q.txt\"",
            f"\\cp {O}/x {M}/bs.txt",
            f"/bin/cp {O}/x {M}/abs.txt",
            f"gcp {O}/x {M}/g.txt",
            f"install {O}/x {M}/i.txt",
            f"install -m 755 {O}/x {M}/i.txt",
            f"ginstall -D {O}/x {M}/deep/i.txt",
            f"install -d {M}/newdir",
            f"ln -s {O}/x {M}/sym",
            f"ln {O}/x {M}/hard",
            f"mv {W}/f.txt {M}/moved.txt",
            f"mv {O}/x {M}/",
        ])

    def test_target_directory_in_main(self):
        M, W, O = self.M, self.W, self.O
        cmds = []
        for prog in ("cp", "gcp", "mv", "gmv", "install", "ginstall", "ln -s", "gln -s"):
            cmds += [
                f"{prog} -t {M} {O}/x",
                f"{prog} -t{M} {O}/x",
                f"{prog} -vt{M} {O}/x",
                f"{prog} --target-directory={M} {O}/x",
                f"{prog} --target-directory {M} {O}/x",
                f"{prog} --target-d={M} {O}/x",
                f"{prog} --targe {M} {O}/x",
                # GNU permutation: the option after the operands
                f"{prog} {O}/x {W}/f.txt -t {M}",
                f"{prog} {O}/x --target-directory={M}",
            ]
        self.expect(DENY, cmds)

    def test_target_directory_through_variables_and_cwd(self):
        M, O = self.M, self.O
        self.expect(DENY, [
            f"T={M}; cp -t \"$T\" {O}/x",
            f"cd {M} && cp -t . {O}/x",
            f"cd {M} && cp {O}/x .",
            f"cd {M}/sub && cp {O}/x ../up.txt",
        ])

    def test_destination_in_main_through_wrappers(self):
        M, O = self.M, self.O
        self.expect(DENY, [
            f"env cp {O}/x {M}/e.txt",
            f"command cp {O}/x {M}/c.txt",
            f"builtin command cp {O}/x {M}/c.txt",
            f"sudo cp {O}/x {M}/s.txt",
            f"nohup cp -t {M} {O}/x",
            f"time mv {O}/x {M}/t.txt",
            f"timeout 5 cp {O}/x {M}/to.txt",
            f"nice -n 5 install {O}/x {M}/n.txt",
            f"bash -c 'cp {O}/x {M}/b.txt'",
            f"bash -lc \"cp -t {M} {O}/x\"",
            f"eval 'cp {O}/x {M}/ev.txt'",
            f"CMD='cp {O}/x {M}/var.txt'; $CMD",
            f"echo {O}/x | xargs -I{{}} cp {{}} {M}/",
            f"echo {O}/x | xargs cp -t {M}",
            f"find {O} -name x -exec cp {{}} {M}/ \\;",
            f"(cp {O}/x {M}/sub.txt)",
            f"true && cp {O}/x {M}/and.txt",
        ])

    def test_mv_out_of_main_is_refused(self):
        M, W, O = self.M, self.W, self.O
        self.expect(DENY, [
            f"mv {M}/f.txt {W}/",
            f"mv {M}/f.txt {O}/m.txt",
            f"mv -f {M}/f.txt {W}/m.txt",
            f"mv -- {M}/f.txt {W}/m.txt",
            f"mv -t {W} {M}/f.txt",
            f"mv --target-directory={O} {M}/f.txt",
            f"mv {M}/f.txt -t {W}",
            f"gmv {M}/f.txt {W}/",
            f"env mv {M}/f.txt {W}/",
            f"bash -c 'mv {M}/f.txt {W}/'",
            f"rsync --remove-source-files {M}/f.txt {O}/",
        ])
        self.expect(DENY, [f"mv f.txt {self.W}/"], cwds=(self.r.main,))

    def test_hard_links_to_main_files_are_refused(self):
        M, W = self.M, self.W
        self.expect(DENY, [
            f"ln {M}/f.txt {W}/h",
            f"ln -P {M}/f.txt {W}/h",
            f"ln -t {W} {M}/f.txt",
            f"cp -l {M}/f.txt {W}/h",
            f"cp -Rl {M}/sub {W}/hs",
            f"cp --li {M}/f.txt {W}/h",
        ])

    def test_symlinked_dir_into_main_as_destination_is_refused(self):
        # <wt>/tomain -> <main>/sub: writing through it writes main.
        O, W = self.O, self.W
        self.expect(DENY, [
            f"cp {O}/x {W}/tomain/",
            f"cp {O}/x {W}/tomain/y.txt",
            f"cp -t {W}/tomain {O}/x",
            f"mv {O}/x {W}/tomain/",
        ])

    def test_case_folded_main_on_darwin(self):
        if sys.platform != "darwin":
            self.skipTest("case folding is darwin-only")
        up = self.M.replace("mainrepo", "MAINREPO")
        self.expect(DENY, [
            f"cp {self.O}/x {up}/case.txt",
            f"cp -t {up} {self.O}/x",
        ])

    def test_lone_operand_link_lands_in_cwd(self):
        self.expect(DENY, [f"ln -s {self.O}/x", f"ln -s {self.W}/f.txt"],
                    cwds=(self.r.main,))
        self.expect(ALLOW, [f"ln -s {self.M}/f.txt"], cwds=(self.r.wt,))


class OpenDefectsFoundByVerifier(Base):
    """Fail-opens this verification found in the new destination rule. Each
    is marked expectedFailure so the file stays usable as a regression
    suite while the defect is open; when it is fixed the test reports an
    UNEXPECTED SUCCESS and the marker must be removed."""

    @unittest.expectedFailure
    def test_posixly_correct_g_name_stops_option_parsing_at_first_operand(self):
        # GNU getopt with POSIXLY_CORRECT set stops option processing at the
        # first non-option (glibc manual, Using Getopt), so in
        # `gcp SRC -t WT MAIN/d` the `-t` and WT are SOURCES and MAIN/d is
        # the destination. The g-name is read only with GNU permutation, so
        # MAIN/d is taken for a source and the write into main is allowed.
        # The pre-change guard refused this (every operand was judged).
        # (POSIXLY_CORRECT inherited from the session's shell is invisible
        # to this hook; that variant is not asserted here.)
        (self.r.main / "d").mkdir(exist_ok=True)
        M, W, O = self.M, self.W, self.O
        self.expect(DENY, [
            f"POSIXLY_CORRECT=1 gcp {O}/x -t {W} {M}/d",
            f"env POSIXLY_CORRECT=1 gcp {O}/x -t {W} {M}/d",
            f"POSIXLY_CORRECT=1 ginstall {O}/x -t {W} {M}/d",
        ])


class NoOverBlock(Base):
    def test_worktree_and_readonly_commands_stay_allowed(self):
        W, O = self.W, self.O
        self.expect(ALLOW, [
            f"cp {W}/f.txt {W}/f2.txt",
            f"cp -r {W}/sub {W}/sub2",
            f"mv {W}/f.txt {W}/f3.txt",
            f"ln -s f.txt {W}/lnk",
            f"ln {W}/f.txt {W}/hardlocal",
            f"install -m 644 {W}/f.txt {W}/inst.txt",
            f"install -d {W}/d1 {O}/d2",
            f"cp /etc/hosts {O}/hosts",
            f"cp -t {O} /etc/hosts /etc/shells",
            f"mv {O}/x {O}/y",
            f"git -C {W} status",
            f"git -C {W} log --oneline -3",
            "ls -la",
            "cat f.txt",
            f"grep -r a {W}",
        ])
        self.expect(ALLOW, [
            "cp f.txt f.copy",
            "mv f.txt moved.txt",
            "cp -t sub f.txt",
            "ln -s f.txt here",
        ], cwds=(self.r.wt,))


if __name__ == "__main__":
    unittest.main()
