#!/usr/bin/env python3
"""RED repro (2026-11-20, user-ruled false positive): check-test-weakening
returns Undetermined (exit 2) for an intended, human-justified re-anchoring of
macro-generated tests, and no `test-weakening-justified:` line can acknowledge
it -- the only way through is a bypass.

Shape (from crates/blastguard/tests/scope_redirect_relaxation_boundaries_20261108.rs,
backlog 266b5ded): an integration test file that exists at the base holds a
`macro_rules!` test template asserting a restrictive decision. Three of its
invocations are replaced by invocations of a NEW template asserting a different
expected value (Allow), and two neighbouring invocations move up.

Observed verbatim: "check-test-weakening: undetermined — crates/blastguard/tests/
scope_redirect_relaxation_boundaries_20261108.rs: #[test] inside a macro_rules!
template in a file that exists at the base -- the change is not one of the
proven-safe shapes (invocation counting included), so removed generated tests
cannot be ruled out".

Property under test:
  * A macro-template change that is not proven safe must never be silently
    clean: without a justified line the gate exits non-zero.
  * The gate must report it as a finding with a kind (one of the
    acknowledgment vocabulary) for that path, so that a matching
    `test-weakening-justified: <path>:<kind> - <reason>` line naming the path
    and the kind the gate reports makes it acknowledgeable: exit 0 with an
    `[ack]` finding.
  * A justified line naming a different path acknowledges nothing.

The RED test reads the kind from the gate's own `--json` output of the
unjustified run rather than fixing one here: which kind the gate reports for
this case is the implementer's decision; that it reports one, and that naming
it acknowledges it, is the property.

Repro: python3 -m pytest scripts/test_check_test_weakening_macro_reanchor_20261120.py -q
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

import pytest

SCRIPT = Path(__file__).resolve().parent / "check-test-weakening.py"
REL = "crates/c/tests/reanchor_repro.rs"
ALL_KINDS = ("assertion-removed", "test-removed", "ignore-added", "should-panic-added")

HEADER = """\
fn py(fx: &Fx, prog: &str) -> (String, String) {
    fx.run(prog)
}

fn assert_restrictive(name: &str, r: &(String, String)) {
    assert!(r.0 == "deny" || r.0 == "ask", "{}: {:?}", name, r);
}

macro_rules! py_hostile {
    ($(#[$attr:meta])* $name:ident, $prog:expr) => {
        $(#[$attr])*
        #[test]
        fn $name() {
            let fx = Fx::new(stringify!($name));
            let r = py(&fx, $prog);
            assert_restrictive(stringify!($name), &r);
        }
    };
}

py_hostile!(py_os_popen, "import os; os.popen(\\"echo x > TARGET\\")");
py_hostile!(
    py_unterminated_triple_quote,
    "import sys; s=\\"\\"\\"> TARGET; print(s)"
);
"""

TAIL = """
/// A real shell redirect OUTSIDE the quoted program must stay restrictive.
#[test]
fn py_real_shell_redirect_after_program_is_not_allowed() {
    let fx = Fx::new("py-real-redirect");
    let r = fx.bash("python3 -c 'print(1)' > TARGET");
    assert_restrictive("real redirect after python -c", &r);
}
"""

BEFORE_MID = """\
py_hostile!(
    py_non_allowlisted_import_multiprocessing,
    "import multiprocessing; s=\\"> TARGET\\"; print(s)"
);
py_hostile!(py_exec_call, "exec(\\"print(1)\\"); s=\\"> TARGET\\"");
py_hostile!(py_eval_call, "s=\\"> TARGET\\"; eval(\\"1\\")");
py_hostile!(
    py_payload_with_command_substitution,
    "import sys; s=\\"> TARGET $(id)\\"; print(s)"
);
py_hostile!(
    py_payload_with_backtick,
    "import sys; s=\\"> TARGET `id`\\"; print(s)"
);
"""

# Mirrors the 266b reanchor patch: exec/eval move up, a new template asserting
# Allow is added, and the three invocations switch to it.
AFTER_MID = """\
py_hostile!(py_exec_call, "exec(\\"print(1)\\"); s=\\"> TARGET\\"");
py_hostile!(py_eval_call, "s=\\"> TARGET\\"; eval(\\"1\\")");

// User ruling 2026-11-20 (backlog 266b5ded): these programs only build and
// print a string; they were Ask under the legacy string readings only.
macro_rules! py_data_only {
    ($name:ident, $prog:expr) => {
        #[test]
        fn $name() {
            let fx = Fx::new(stringify!($name));
            let r = py(&fx, $prog);
            assert_eq!(
                r.0,
                "allow",
                "{}: a program that only prints a string is Allow; got decision JSON: {:?}",
                stringify!($name),
                r.1.trim()
            );
        }
    };
}

py_data_only!(
    py_non_allowlisted_import_multiprocessing,
    "import multiprocessing; s=\\"> TARGET\\"; print(s)"
);
py_data_only!(
    py_payload_with_command_substitution,
    "import sys; s=\\"> TARGET $(id)\\"; print(s)"
);
py_data_only!(
    py_payload_with_backtick,
    "import sys; s=\\"> TARGET `id`\\"; print(s)"
);
"""

BEFORE = HEADER + BEFORE_MID + TAIL
AFTER = HEADER + AFTER_MID + TAIL

RED_REASON = (
    "backlog (id pending): an intended macro-template re-anchor in a file that exists "
    "at the base is Undetermined and no test-weakening-justified line can "
    "acknowledge it (user-ruled false positive 2026-11-20)"
)


class MacroReanchor(unittest.TestCase):
    def setUp(self) -> None:
        td = tempfile.TemporaryDirectory(prefix="twmacroreanchor.")
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
        self.file = self.repo / REL
        self.file.write_text(BEFORE)
        self.git("add", "-A")
        self.git("commit", "-q", "-m", "C0")
        self.git("checkout", "-q", "-b", "feat")
        self.file.write_text(AFTER)
        self.git("add", "-A")
        self.assertIn(REL, self.git("diff", "--cached", "--name-only"),
                      "anti-vacuity: the change is staged")
        self.assertNotEqual(BEFORE, AFTER)
        self.assertEqual(BEFORE.count("py_hostile!(") - 3, AFTER.count("py_hostile!("),
                         "anti-vacuity: three restrictive invocations are replaced")

    def git(self, *args: str) -> str:
        return subprocess.run(["git", *args], cwd=self.repo, env=self.env, check=True,
                              capture_output=True, text=True).stdout

    def scan(self, message: str, *extra: str) -> tuple[int, str, str]:
        msg = self.tmp / "COMMIT_EDITMSG"
        msg.write_text(message)
        r = subprocess.run([sys.executable, str(SCRIPT), "--repo", str(self.repo),
                            "--pending-msg", str(msg), *extra],
                           cwd=self.repo, env=self.env, capture_output=True,
                           text=True, timeout=60)
        return r.returncode, r.stdout, r.stderr

    @pytest.mark.xfail(strict=True, reason=RED_REASON)
    def test_red_justified_line_acknowledges_the_reanchor(self) -> None:
        # The gate must report this change as a finding (with a kind) for the
        # path; it must not be silently clean.
        rc, out, err = self.scan("re-anchor three tests\n", "--json")
        self.assertNotEqual(rc, 0, out + err)
        verdict = json.loads(out.strip().splitlines()[-1])
        kinds = sorted({f["kind"] for f in verdict["findings"] if f["path"] == REL})
        self.assertTrue(kinds, "no finding (kind) reported for the path, so no "
                        "justified line can name it; rc={} out={!r} err={!r}"
                        .format(rc, out, err))
        for k in kinds:
            self.assertIn(k, ALL_KINDS)
        # Naming the path and the reported kind(s) must acknowledge it.
        message = "re-anchor three tests\n\n" + "".join(
            f"test-weakening-justified: {REL}:{k} - user ruling 2026-11-20: "
            "these programs only print a string, Allow is correct\n"
            for k in kinds
        )
        rc, out, err = self.scan(message)
        self.assertEqual(rc, 0, out + err)
        self.assertIn(f"[ack] {REL}:", out)
        self.assertNotIn("[BLOCK]", out)

    def test_control_without_justified_line_is_nonzero(self) -> None:
        rc, out, err = self.scan("re-anchor three tests\n")
        self.assertNotEqual(rc, 0, out + err)

    def test_control_justified_line_for_other_path_is_nonzero(self) -> None:
        message = "re-anchor three tests\n\n" + "".join(
            f"test-weakening-justified: crates/c/tests/other.rs:{k} - unrelated\n"
            for k in ALL_KINDS
        )
        rc, out, err = self.scan(message)
        self.assertNotEqual(rc, 0, out + err)


if __name__ == "__main__":
    unittest.main()
