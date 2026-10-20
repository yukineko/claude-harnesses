#!/usr/bin/env python3
"""RED repro for backlog 207d0362.

207d0362: a `.claude-plugin/marketplace.json` plugin entry whose `description`
opens with a `vX.Y.Z` release tag keeps citing an OLDER release than the
entry's own `version`, and no gate catches it (check-plugin-versions.py checks
the three version fields for lockstep, never the description prose). A reader
cannot tell whether the described behaviour is the shipped behaviour.

Pinned here:
  * control: the scan sees tagged descriptions at all (an empty scan would
    pass for the wrong reason);
  * defect (RED): every description that opens with `vX.Y.Z` names the entry's
    current `version`.

NOTE for the human decision the item asks for: if the description is instead
declared a cumulative changelog whose leading tag is NOT meant to track
`version`, this assertion is the wrong contract and must be replaced, not
silenced.
"""

from __future__ import annotations

import json
import os
import re
import unittest

SCRIPTS = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(SCRIPTS)
MARKETPLACE = os.path.join(REPO, ".claude-plugin", "marketplace.json")
TAG = re.compile(r"\s*v(\d+\.\d+\.\d+)")


def tagged() -> list[tuple[str, str, str]]:
    with open(MARKETPLACE, encoding="utf-8") as f:
        m = json.load(f)
    out = []
    for p in m["plugins"]:
        mm = TAG.match(p.get("description", ""))
        if mm:
            out.append((p["name"], mm.group(1), p.get("version", "")))
    return out


class MarketplaceDescriptionTagTracksVersion(unittest.TestCase):
    def test_scan_sees_tagged_descriptions(self) -> None:
        self.assertTrue(tagged(), "no description opens with a vX.Y.Z tag")

    @unittest.expectedFailure  # backlog 207d0362: open defect
    def test_leading_tag_equals_version(self) -> None:
        drift = [f"{n}: description v{t} vs version {v}" for n, t, v in tagged() if t != v]
        self.assertEqual(drift, [], f"{len(drift)} drifted entries:\n" + "\n".join(drift))


if __name__ == "__main__":
    unittest.main()
