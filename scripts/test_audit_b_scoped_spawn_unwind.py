#!/usr/bin/env python3
"""Audit B item 9 (memory: thread-scope-reraises-panic).

`std::thread::scope` re-raises the panic of any auto-joined scoped worker when
the scope closes, so one worker's panic tears down the gate instead of being
folded as a failed slot (verified 2026-07-02). The memory's rule — wrap every
scoped worker body in `catch_unwind(AssertUnwindSafe(..))` — is applied by
recall only; nothing refuses a new scoped spawn without it.

Property asserted (scanner-name agnostic): some scanner wired into
`.githooks/pre-commit` exits non-zero on a staged diff that adds, in a GATE
crate's src, a `thread::scope` whose `s.spawn` body lacks `catch_unwind`, while
exiting 0 on the identical diff with the body wrapped in
`std::panic::catch_unwind(std::panic::AssertUnwindSafe(..))`. See
scripts/audit_b_precommit_fixture.py for the differential.

Ratchet guard (passes today, must keep passing): an unwrapped scoped spawn
already present at the base commit, with an unrelated edit staged, is not
refused by any scanner that accepts the clean control.
"""
import unittest

from scripts import audit_b_precommit_fixture as F

CRATE = "blastguard"  # one of the GATE crates
SRC = "src/store.rs"

TEMPLATE = (
    "pub fn fan(items: &[u32], out: &Sink) {\n"
    "    std::thread::scope(|s| {\n"
    "        for &i in items {\n"
    "            s.spawn(move || record(out, BODY));\n"
    "        }\n"
    "    });\n"
    "}\n"
)
BARE = "work(i)"
WRAPPED = "std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(i)))"
BASE = "pub fn other() -> u32 {\n    1\n}\n"


def _src(body, extra=""):
    return BASE + extra + "\n" + TEMPLATE.replace("BODY", body)


class ScopedSpawnNeedsCatchUnwind(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.control = F.run_scanners(CRATE, SRC, BASE, _src(WRAPPED))

    def test_control_some_scanner_accepts_wrapped_spawn(self):
        self.assertTrue(F.accepting(self.control), self.control)

    @unittest.expectedFailure  # backlog 4620134f: open defect
    def test_added_scoped_spawn_without_catch_unwind_is_refused(self):
        control = self.control
        refusing, bad = F.refusing_scanners(CRATE, SRC, BASE, _src(BARE), control)
        ok = F.accepting(control)
        self.assertTrue(
            refusing,
            "no pre-commit scanner refuses a newly added thread::scope spawn "
            f"without catch_unwind in crates/{CRATE}/{SRC}. Scanners accepting "
            f"the wrapped control: {ok}; their exit codes on the bare diff: "
            f"{ {s: bad[s][0] for s in ok} }",
        )

    def test_ratchet_preexisting_bare_spawn_is_not_refused(self):
        base = _src(BARE)
        staged = base.replace("    1\n}", "    2\n}", 1)
        clean = F.run_scanners(CRATE, SRC, BASE, BASE.replace("    1\n}", "    2\n}", 1))
        ok = F.accepting(clean)
        pre = F.run_scanners(CRATE, SRC, base, staged, only=set(ok))
        blocked = [s for s in ok if pre[s][0] != 0]
        self.assertEqual(blocked, [], "a pre-existing scoped spawn must be counted, not refused")


if __name__ == "__main__":
    unittest.main()
