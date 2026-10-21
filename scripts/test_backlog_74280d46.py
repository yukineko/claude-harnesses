#!/usr/bin/env python3
"""backlog 74280d46: scripts/check-unlisted-gates.py counts an invocation that
can never execute as "invoked". A scanner called only inside a function that is
never called, under `if false; then`, or after an unconditional `exit 0` reads
as listed (exit 0), while no commit ever runs it: the fail-open direction
(dark, not red). The scanner's docstring states this as a KNOWN LIMITATION.

Each case drives the real scanner on a throwaway repo (the Fx fixture of
scripts/test_check_unlisted_gates.py). The property asserted is CLAUDE.md 3's:
the scanner must not answer the clean code (0) for a hook in which the only
mention of check-dead.py is unreachable; either "unlisted" (1) or
"undetermined" (2) is acceptable.

Control: the same hook with the call reachable is clean (0), so the fixture
can observe GREEN and a RED below is about reachability alone.

Repro:  python3 -m pytest --runxfail scripts/test_backlog_74280d46.py
"""

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import test_check_unlisted_gates as tcu  # noqa: E402

UNREACHABLE = {
    "uncalled_function": ["dead() {", "    run check-dead.py dead", "}"],
    "if_false": ["if false; then", "    run check-dead.py dead", "fi"],
    "after_exit_0": ["exit 0", "run check-dead.py dead"],
}


def _check(test, lines):
    fx = tcu.Fx(test)
    fx.write_hook(fx.main, "pre-commit", tcu.hook(["run check-a.py a", *lines]))
    fx.add_scanner(fx.main, "check-a.py")
    fx.add_scanner(fx.main, "check-dead.py")
    fx.set_hooks_path("absolute")
    return fx.check(fx.main)


class UnreachableInvocationIsNotListed(unittest.TestCase):
    def test_control_reachable_call_is_clean(self):
        p = _check(self, ["run check-dead.py dead"])
        self.assertEqual(p.returncode, 0, tcu.out(p))

    def test_unreachable_call_is_not_counted_as_invoked(self):
        for name, lines in UNREACHABLE.items():
            with self.subTest(case=name):
                p = _check(self, lines)
                self.assertNotEqual(
                    p.returncode,
                    0,
                    "%s: check-dead.py can never run, yet the scanner reports "
                    "every gate invoked\n%s" % (name, tcu.out(p)),
                )


if __name__ == "__main__":
    unittest.main(verbosity=1)
