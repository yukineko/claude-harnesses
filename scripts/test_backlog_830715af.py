#!/usr/bin/env python3
"""Repro for backlog 830715af: schemaguard's README (en/ja) and its
marketplace description enumerate a stale schema set (4 names) while
`registry::names()` declares 6 (adds `undetermined-probe` and `verdict`); the
marketplace description also carries a stale version note ("v0.1.6: ...").

Settled the way the ticket asks: diff registry::names() (parsed from the
source the `list` subcommand prints) against each description.
"""
import json
import re
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
REGISTRY = REPO / "crates/schemaguard/src/registry.rs"


def registry_names() -> list:
    src = REGISTRY.read_text(encoding="utf-8")
    m = re.search(r"pub fn names\(\)[^{]*\{\s*vec!\[(.*?)\]", src, re.S)
    if not m:
        raise AssertionError("cannot locate registry::names() -- undetermined, not clean")
    names = re.findall(r'"([a-z0-9-]+)"', m.group(1))
    if not names:
        raise AssertionError("registry::names() parsed empty -- undetermined, not clean")
    return names


def marketplace_entry() -> dict:
    m = json.loads((REPO / ".claude-plugin/marketplace.json").read_text(encoding="utf-8"))
    for p in m["plugins"]:
        if p["name"] == "schemaguard":
            return p
    raise AssertionError("no schemaguard entry -- undetermined, not clean")


class SchemaguardDescriptions(unittest.TestCase):
    def test_control_registry_has_six(self):
        self.assertEqual(len(registry_names()), 6, registry_names())

    @unittest.expectedFailure  # backlog 830715af: open defect, remove when fixed
    def test_readme_en_lists_every_registered_schema(self):
        t = (REPO / "crates/schemaguard/README.md").read_text(encoding="utf-8")
        line = next(l for l in t.splitlines() if l.startswith("Declared schemas"))
        missing = [n for n in registry_names() if f"`{n}`" not in line]
        self.assertEqual(missing, [], f"README.md '{line}' omits {missing}")

    @unittest.expectedFailure  # backlog 830715af: open defect, remove when fixed
    def test_readme_ja_lists_every_registered_schema(self):
        t = (REPO / "crates/schemaguard/README.ja.md").read_text(encoding="utf-8")
        line = next(l for l in t.splitlines() if l.startswith("宣言済みスキーマは"))
        missing = [n for n in registry_names() if f"`{n}`" not in line]
        self.assertEqual(missing, [], f"README.ja.md '{line}' omits {missing}")

    @unittest.expectedFailure  # backlog 830715af: open defect, remove when fixed
    def test_marketplace_description_is_current(self):
        e = marketplace_entry()
        d = e["description"]
        stale_versions = [v for v in re.findall(r"v(\d+\.\d+\.\d+)", d) if v != e["version"]]
        missing = [n for n in ("verdict", "undetermined-probe") if n not in d]
        self.assertEqual((stale_versions, missing), ([], []),
                         f"marketplace description: stale version notes {stale_versions}, "
                         f"omits schemas {missing}: {d}")


if __name__ == "__main__":
    unittest.main()
