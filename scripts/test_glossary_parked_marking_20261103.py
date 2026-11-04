"""INTENTIONALLY RED repro (filed 2026-11-03): docs/GLOSSARY.md does not mark
every parked plugin as parked.

scripts/parked-plugins.json is the canonical list of plugins deliberately left
un-rolled-out / un-enabled (read by scripts/check-plugin-rollout.py). GLOSSARY's
crate table marks `fde` with `（parked）` but the `parallelguard` row — parked
for ENABLEMENT since 2026-08-28 — reads `always-on` and describes the gate as
denying calls, with no parked marking. No checker ties GLOSSARY to
parked-plugins.json, so the drift is invisible.

Property asserted: every key of parked-plugins.json["parked"] has a GLOSSARY
crate-table row, and that row contains the word "parked". Plain RED (no
expectedFailure) because `backlog add --repro-test` requires a behavioural
failure at HEAD. Stdlib only, read-only.
"""
import json
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent


def parked_plugins():
    data = json.loads((REPO / "scripts" / "parked-plugins.json").read_text(encoding="utf-8"))
    return sorted(data["parked"].keys())


def glossary_rows():
    text = (REPO / "docs" / "GLOSSARY.md").read_text(encoding="utf-8")
    rows = {}
    for line in text.splitlines():
        if not line.startswith("| "):
            continue
        cells = [c.strip() for c in line.strip("|").split("|")]
        if cells:
            rows[cells[0]] = line
    return rows


class GlossaryParkedMarking(unittest.TestCase):
    def test_control_parked_list_is_non_empty_and_every_entry_has_a_row(self):
        parked = parked_plugins()
        self.assertTrue(parked, "parked-plugins.json lists no parked plugin")
        rows = glossary_rows()
        missing = [p for p in parked if p not in rows]
        self.assertEqual(missing, [], f"parked plugins with no GLOSSARY row: {missing}")

    def test_every_parked_plugin_row_is_marked_parked(self):
        rows = glossary_rows()
        unmarked = [p for p in parked_plugins() if p in rows and "parked" not in rows[p].lower()]
        self.assertEqual(
            unmarked,
            [],
            f"GLOSSARY rows for parked plugins carry no parked marking: {unmarked}",
        )


if __name__ == "__main__":
    unittest.main()
