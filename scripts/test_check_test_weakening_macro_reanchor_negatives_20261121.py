#!/usr/bin/env python3
"""Negatives for backlog 74810d3e (independent test author, 2026-11-21).

`template_reanchor_detail` in check-test-weakening.py turns ONE shape into an
acknowledgeable `assertion-removed` finding: invocations move from one
`macro_rules!` test template to another and NOTHING else changes. Each
re-anchor combined with one extra change must stay Undetermined (rc 2,
verdict "undetermined", no finding), and a `test-weakening-justified:` line
must not be able to turn it into rc 0.

Run: pytest scripts/test_check_test_weakening_macro_reanchor_negatives_20261121.py
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "check-test-weakening.py"
REL = "crates/c/tests/reanchor_negatives.rs"
JUSTIFIED = f"test-weakening-justified: {REL}:assertion-removed - x\n"
ALL_KINDS = ("assertion-removed", "test-removed", "ignore-added", "should-panic-added")
JUSTIFIED_ALL = "".join(f"test-weakening-justified: {REL}:{k} - x\n" for k in ALL_KINDS)

HEAD = """\
fn run(fx: &Fx, prog: &str) -> (String, String) {
    fx.run(prog)
}

macro_rules! hostile {
    ($name:ident, $prog:expr) => {
        #[test]
        fn $name() {
            let fx = Fx::new(stringify!($name));
            let r = run(&fx, $prog);
            assert!(r.0 == "deny" || r.0 == "ask", "{:?}", r);
        }
    };
}

"""

INVS = {
    "one": 'hostile!(case_one, "print(1)");\n',
    "two": 'hostile!(case_two, "print(2)");\n',
    "three": 'hostile!(case_three, "print(3)");\n',
}


def base_src() -> str:
    return HEAD + INVS["one"] + INVS["two"] + INVS["three"]


ALLOW_TEMPLATE = """\
macro_rules! allowed {
    ($name:ident, $prog:expr) => {
        #[test]
        fn $name() {
            let fx = Fx::new(stringify!($name));
            let r = run(&fx, $prog);
            assert_eq!(r.0, "allow", "{:?}", r);
        }
    };
}

"""


def reanchored(*, drop_three=False, edit_two=False, helper=False,
               edit_base_template=False) -> str:
    """The accepted re-anchor (case_one -> allowed!) plus at most one extra."""
    head = HEAD
    if edit_base_template:
        head = head.replace('"deny" || r.0 == "ask"',
                            '"deny" || r.0 == "ask" || r.0 == "allow"')
        assert head != HEAD
    s = head
    if helper:
        s += "fn helper_added() -> u32 {\n    7\n}\n\n"
    s += ALLOW_TEMPLATE
    one = INVS["one"].replace("hostile!", "allowed!")
    two = INVS["two"].replace("print(2)", "print(22)") if edit_two else INVS["two"]
    three = "" if drop_three else INVS["three"]
    return s + one + two + three


class Base(unittest.TestCase):
    def setUp(self) -> None:
        td = tempfile.TemporaryDirectory(prefix="twreanchorneg.")
        self.addCleanup(td.cleanup)
        self.tmp = Path(os.path.realpath(td.name))
        home = self.tmp / "home"
        home.mkdir()
        self.env = {
            "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@example.invalid",
            "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@example.invalid",
            "GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1",
            "HOME": str(home), "TMPDIR": str(self.tmp), "LC_ALL": "C",
            "PATH": os.environ.get("PATH", ""),
        }
        self.repo = self.tmp / "repo"
        (self.repo / "crates" / "c" / "tests").mkdir(parents=True)
        self.git("init", "-q", "-b", "main")
        self.file = self.repo / REL
        self.file.write_text(base_src())
        self.git("add", "-A")
        self.git("commit", "-q", "-m", "C0")
        self.git("checkout", "-q", "-b", "feat")

    def git(self, *args: str) -> str:
        return subprocess.run(["git", *args], cwd=self.repo, env=self.env, check=True,
                              capture_output=True, text=True).stdout

    def stage(self, after: str) -> None:
        self.assertNotEqual(after, base_src(), "anti-vacuity: the change is a change")
        self.file.write_text(after)
        self.git("add", "-A")
        self.assertIn(REL, self.git("diff", "--cached", "--name-only"))

    def scan(self, message: str, *extra: str) -> tuple[int, str, str]:
        msg = self.tmp / "COMMIT_EDITMSG"
        msg.write_text(message)
        r = subprocess.run([sys.executable, str(SCRIPT), "--repo", str(self.repo),
                            "--pending-msg", str(msg), "--json", *extra],
                           cwd=self.repo, env=self.env, capture_output=True,
                           text=True, timeout=60)
        return r.returncode, r.stdout, r.stderr

    def assert_undetermined(self, after: str) -> None:
        self.stage(after)
        rc, out, err = self.scan("change\n")
        self.assertEqual(rc, 2, out + err)
        v = json.loads(out.strip().splitlines()[-1])
        self.assertEqual(v["verdict"], "undetermined", out)
        self.assertFalse(v.get("findings"), out)
        # A justified line must not acknowledge an Undetermined verdict.
        rc, out, err = self.scan("change\n\n" + JUSTIFIED)
        self.assertNotEqual(rc, 0, out + err)
        self.assertEqual(rc, 2, out + err)
        v = json.loads(out.strip().splitlines()[-1])
        self.assertEqual(v["verdict"], "undetermined", out)
        # Nor can naming every kind for the path.
        rc, out, err = self.scan("change\n\n" + JUSTIFIED_ALL)
        self.assertEqual(rc, 2, out + err)


class PositiveControl(Base):
    def test_exact_reanchor_is_a_finding_and_ackable(self) -> None:
        self.stage(reanchored())
        rc, out, err = self.scan("change\n")
        self.assertNotEqual(rc, 2, out + err)
        self.assertEqual(rc, 1, out + err)
        v = json.loads(out.strip().splitlines()[-1])
        kinds = {(f["path"], f["kind"]) for f in v["findings"]}
        self.assertIn((REL, "assertion-removed"), kinds, out)
        # The accepted shape also reports `test-removed` (hostile! 3 -> 2); a
        # justified line must name every kind the gate reports.
        reported = sorted({f["kind"] for f in v["findings"] if f["path"] == REL})
        msg = "change\n\n" + "".join(
            f"test-weakening-justified: {REL}:{k} - x\n" for k in reported)
        rc, out, err = self.scan(msg)
        self.assertEqual(rc, 0, out + err)
        # ...and the assertion-removed line alone is not enough (observed).
        rc, out, err = self.scan("change\n\n" + JUSTIFIED)
        self.assertEqual(rc, 1, out + err)


class ReanchorPlusExtra(Base):
    def test_a_invocation_also_dropped(self) -> None:
        self.assert_undetermined(reanchored(drop_three=True))

    def test_b_invocation_argument_also_edited(self) -> None:
        self.assert_undetermined(reanchored(edit_two=True))

    def test_c_pure_reorder_without_template_change(self) -> None:
        self.assert_undetermined(HEAD + INVS["two"] + INVS["three"] + INVS["one"])

    def test_d_new_helper_item_also_added(self) -> None:
        self.assert_undetermined(reanchored(helper=True))

    def test_e_base_template_body_also_edited(self) -> None:
        self.assert_undetermined(reanchored(edit_base_template=True))


if __name__ == "__main__":
    unittest.main()
