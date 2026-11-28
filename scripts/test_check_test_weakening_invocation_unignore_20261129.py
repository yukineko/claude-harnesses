#!/usr/bin/env python3
"""RED repro (2026-11-29): check-test-weakening raises Undetermined (exit 2) for
a commit whose ONLY test change is removing a leading `#[ignore = "..."]`
attribute from the ARGUMENTS of macro invocations, where the macro template
forwards it via `$(#[$m:meta])*` (shape of
crates/blastguard/tests/verifier_5c848034_r3_20261129.rs, observed false block
2026-11-29 while un-ignoring verifier REDs). Invocation count, names and every
other argument are unchanged, so no generated test is removed or weakened; the
gate must exit 0.

Observed gate output on the real file (verbatim):
  check-test-weakening: undetermined — ...: #[test] inside a macro_rules!
  template in a file that exists at the base -- the change is not one of the
  proven-safe shapes (invocation counting included), so removed generated
  tests cannot be ruled out

Convention: like its siblings (test_check_test_weakening_macro_unignore*.py),
the target is a plain failing test (no expectedFailure) until the gate is fixed.

Controls (must hold today and after the fix):
  (a) removing one invocation               -> non-zero
  (b) adding `#[ignore]` to an invocation   -> non-zero
  (c) ignore removal + changing another arg (expected-decision string)
                                            -> non-zero (must not become 0)
  (d) ignore removal + renaming the test ident -> non-zero

Repro: python3 -m pytest scripts/test_check_test_weakening_invocation_unignore_20261129.py -q
"""
from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "check-test-weakening.py"

HEADER = """\
fn same_as_plain(setup: &str, cwd: &str, u: &str, q: &str) {
    assert_eq!(decide(setup, cwd, u), decide(setup, cwd, q));
    assert!(!u.is_empty());
}

macro_rules! sys {
    ($(#[$m:meta])* $name:ident: $setup:expr, $cwd:expr, $u:expr, $q:expr) => {
        $(#[$m])*
        #[test]
        fn $name() {
            same_as_plain($setup, $cwd, $u, $q);
        }
    };
}

"""

IGN_A = '#[ignore = "RED(badf720d): `cp x /et?/f` is ask while `cp x /etc/f` is deny"] '
IGN_B = '#[ignore = "RED(badf720d): `cp x /[e]tc/f` is ask (base same)"] '

# (ignore_prefix_or_None, name, setup, cwd, u, q)
ROWS = [
    (None, "sys_cp_plain", '""', '"{R}"', '"cp a /etc/x"', '"cp a \\\\/etc/x"'),
    (IGN_A, "sys_cp_glob_dir_q", '""', '"{R}"', '"cp a /etc/x"', '"cp a /et?/x"'),
    (IGN_B, "sys_cp_glob_dir_class", '""', '"{R}"', '"cp a /etc/x"', '"cp a /[e]tc/x"'),
    (None, "sys_cp_ansi_c", '""', '"{R}"', '"cp a /etc/x"', "\"cp a $'/etc'/x\""),
]


def inv(ign: str | None, name: str, setup: str, cwd: str, u: str, q: str) -> str:
    return f"sys!({ign or ''}{name}: {setup}, {cwd}, {u}, {q});\n"


def render(rows) -> str:
    return HEADER + "".join(inv(*r) for r in rows)


def unignored(rows):
    return [(None, *r[1:]) for r in rows]


class InvocationUnignore(unittest.TestCase):
    def setUp(self) -> None:
        td = tempfile.TemporaryDirectory(prefix="twinv.")
        self.addCleanup(td.cleanup)
        self.tmp = Path(os.path.realpath(td.name))
        home = self.tmp / "home"
        home.mkdir()
        self.env = {
            "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@example.invalid",
            "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@example.invalid",
            "GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1",
            "HOME": str(home), "TMPDIR": str(self.tmp), "LC_ALL": "C",
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        }
        self.repo = self.tmp / "repo"
        (self.repo / "crates" / "c" / "tests").mkdir(parents=True)
        self.git("init", "-q", "-b", "main")
        self.file = self.repo / "crates" / "c" / "tests" / "inv_repro.rs"

    def git(self, *args: str) -> str:
        return subprocess.run(["git", *args], cwd=self.repo, env=self.env, check=True,
                              capture_output=True, text=True).stdout

    def base(self, content: str) -> None:
        self.file.write_text(content)
        self.git("add", "-A")
        self.git("commit", "-q", "-m", "C0")
        self.git("checkout", "-q", "-b", "feat")

    def change(self, content: str) -> None:
        self.file.write_text(content)
        self.git("add", "-A")
        self.assertIn("inv_repro.rs", self.git("diff", "--cached", "--name-only"),
                      "anti-vacuity: the change is staged")

    def scan(self) -> tuple[int, str]:
        msg = self.tmp / "COMMIT_EDITMSG"
        msg.write_text("un-ignore macro invocations\n")
        r = subprocess.run([sys.executable, str(SCRIPT), "--repo", str(self.repo),
                            "--pending-msg", str(msg)],
                           cwd=self.repo, env=self.env, capture_output=True,
                           text=True, timeout=60)
        return r.returncode, r.stdout + r.stderr

    # ---- target (RED today: exit 2 Undetermined)
    def test_target_removing_only_invocation_ignore_passes(self) -> None:
        self.base(render(ROWS))
        after = render(unignored(ROWS))
        # anti-vacuity: the diff is exactly the two ignore prefixes
        self.assertEqual(after, render(ROWS).replace(IGN_A, "").replace(IGN_B, ""))
        self.change(after)
        rc, out = self.scan()
        self.assertEqual(rc, 0, out)

    # ---- controls
    def test_control_a_removing_one_invocation_blocks(self) -> None:
        self.base(render(ROWS))
        self.change(render(ROWS[:-1]))
        rc, out = self.scan()
        self.assertNotEqual(rc, 0, out)

    def test_control_b_adding_invocation_ignore_blocks(self) -> None:
        self.base(render(ROWS))
        rows = list(ROWS)
        rows[0] = (IGN_A, *rows[0][1:])
        self.change(render(rows))
        rc, out = self.scan()
        self.assertNotEqual(rc, 0, out)

    def test_control_c_unignore_plus_arg_change_not_clean(self) -> None:
        self.base(render(ROWS))
        rows = unignored(ROWS)
        r = rows[1]
        rows[1] = (r[0], r[1], r[2], r[3], r[4], '"cp a /etc/x"')  # q := u (vacuous)
        self.change(render(rows))
        rc, out = self.scan()
        self.assertNotEqual(rc, 0, out)

    def test_control_d_unignore_plus_rename_ident_not_clean(self) -> None:
        self.base(render(ROWS))
        rows = unignored(ROWS)
        r = rows[1]
        rows[1] = (r[0], "sys_cp_glob_dir_q_renamed", *r[2:])
        self.change(render(rows))
        rc, out = self.scan()
        self.assertNotEqual(rc, 0, out)


if __name__ == "__main__":
    unittest.main()
