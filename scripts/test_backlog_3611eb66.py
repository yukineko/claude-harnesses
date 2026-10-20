#!/usr/bin/env python3
"""Repro for backlog 3611eb66.

scripts/build-plugin-bin.sh's header says "The committed per-platform binaries
are what `/plugin install` ships", while .gitignore ignores exactly those files
(crates/*/bin/*-<os>-<arch>, "NEVER committed"). The prose contradicts the
repository's own configuration (CLAUDE.md section 4).

Property asserted: while .gitignore ignores the per-platform binaries, the
build script's header does not describe them as committed.

Written by an independent auditor, not an implementer.
"""

from __future__ import annotations

import os
import re
import unittest

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SCRIPT = os.path.join(REPO, "scripts", "build-plugin-bin.sh")
GITIGNORE = os.path.join(REPO, ".gitignore")


def _header() -> str:
    lines = []
    with open(SCRIPT, encoding="utf-8") as f:
        for line in f:
            if line.startswith("#"):
                lines.append(line.lstrip("#").strip())
            elif line.strip():
                break
    return " ".join(lines)


class BuildScriptHeader(unittest.TestCase):
    def test_precondition_gitignore_ignores_platform_binaries(self) -> None:
        with open(GITIGNORE, encoding="utf-8") as f:
            text = f.read()
        self.assertIn("crates/*/bin/*-linux-x86_64", text)
        self.assertIn("crates/*/bin/*-darwin-arm64", text)

    @unittest.expectedFailure  # backlog 3611eb66: open defect
    def test_header_does_not_claim_binaries_are_committed(self) -> None:
        header = _header()
        self.assertIsNone(
            re.search(r"\bcommitted per-platform binaries\b", header),
            f"header claims committed binaries that .gitignore never commits: {header!r}",
        )


if __name__ == "__main__":
    unittest.main()
