#!/usr/bin/env python3
"""RED repro for backlog 0c277117.

`specguard map gate-check` exists (51151bed: a changed gate-crate map entry with
neither a spec_doc nor a reasoned ack exits 1, undetermined exits 2) and the
bindings were filled (.specguard/spec-docs.toml / spec-doc-acks.toml), but no
local hook calls it: specguard still blocks nowhere — not at commit, push or
Stop. The remaining step of the ticket is the pre-push wiring.

This is a WIRING test (lexical, by necessity): running the real pre-push needs
a full `cargo check --workspace` of the pushed tip, so the observable here is
whether the hook invokes gate-check at all, outside comments. Open defect ->
expectedFailure.

    python3 scripts/test_backlog_0c277117.py
"""

from __future__ import annotations

import re
import unittest
from pathlib import Path

_REPO = Path(__file__).resolve().parent.parent
HOOKS = [_REPO / ".githooks" / "pre-push", _REPO / ".githooks" / "pre-commit"]


def _code_lines(path: Path) -> list[str]:
    out = []
    for line in path.read_text().splitlines():
        stripped = line.lstrip()
        if stripped.startswith("#"):
            continue
        out.append(line)
    return out


class SpecguardGateCheckIsWired(unittest.TestCase):
    def test_hooks_exist_control(self):
        for h in HOOKS:
            self.assertTrue(h.is_file(), "control: missing hook %s" % h)

    @unittest.expectedFailure  # backlog 0c277117: open defect, remove when fixed
    def test_pre_push_invokes_specguard_map_gate_check(self):
        pat = re.compile(r"specguard\b.*\bmap\b.*\bgate-check\b")
        hits = [l for l in _code_lines(HOOKS[0]) if pat.search(l)]
        self.assertTrue(
            hits, ".githooks/pre-push never runs `specguard map gate-check`, so a "
                  "push touching a gate crate with no spec binding is never stopped",
        )


if __name__ == "__main__":
    unittest.main()
