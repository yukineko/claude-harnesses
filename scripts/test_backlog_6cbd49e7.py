"""Repro for backlog 6cbd49e7: docs/GLOSSARY.md's `.githooks/` entry still lists the
removed `taintguard` crate among the pre-push GATE_CRATES.

The canonical set is the `GATE_CRATES=` line of scripts/rollout-plugins.sh (see
scripts/check-gate-crates-sync.py). The GLOSSARY copy is not one of that gate's
SOURCES, so nothing catches the drift. Stdlib only, read-only.
"""
import re
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent


def canonical_gate_crates():
    text = (REPO / "scripts" / "rollout-plugins.sh").read_text(encoding="utf-8")
    m = re.search(r'^GATE_CRATES="([^"]+)"', text, re.M)
    assert m, "GATE_CRATES= line not found in rollout-plugins.sh"
    return set(m.group(1).split())


def glossary_githooks_gate_crates():
    text = (REPO / "docs" / "GLOSSARY.md").read_text(encoding="utf-8")
    line = next((l for l in text.splitlines() if l.startswith("- **`.githooks/`**")), None)
    assert line is not None, "GLOSSARY .githooks entry not found"
    m = re.search(r"GATE_CRATES（([^）]+)）", line)
    assert m, "GLOSSARY .githooks entry no longer enumerates GATE_CRATES"
    return set(m.group(1).split("/"))


class Backlog6cbd49e7(unittest.TestCase):
    @unittest.expectedFailure  # backlog 6cbd49e7: open defect, remove when fixed
    def test_glossary_githooks_gate_crates_match_canonical_set(self):
        self.assertEqual(glossary_githooks_gate_crates(), canonical_gate_crates())

    def test_control_canonical_set_has_no_taintguard(self):
        self.assertNotIn("taintguard", canonical_gate_crates())
        self.assertFalse((REPO / "crates" / "taintguard").exists())


if __name__ == "__main__":
    unittest.main()
