#!/usr/bin/env python3
"""Repro for backlog 9e0c9106: the comment above
`hypothesis_field_set_has_no_metric_surface` in
crates/hypothesis/src/hypothesis.rs claims that a field added with
`#[serde(default)]` lets the struct literal keep compiling. That is false
(serde attributes affect deserialization only; a Rust struct literal must name
every field), and the same test body already says so in an inline NOTE that
contradicts the paragraph above it.

Observable: the comment block still carries the false claim. The control
checks the fact the claim is about: the literal in that very test lists the
two fields t3 added (`scope_write_paths`, `scope_read_paths`), which are
`#[serde(default)]`-style optional fields that nevertheless had to be written
out.

Open defect: expectedFailure keeps the default suite green. Run it RED with
`python3 -m pytest --runxfail scripts/test_backlog_9e0c9106.py`.
"""

import re
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
SRC = REPO / "crates" / "hypothesis" / "src" / "hypothesis.rs"
FALSE_CLAIM = re.compile(
    r"with\s+`#\[serde\(default\)\]`\s+and\s+this\s+struct\s+literal\s+keeps\s+compiling",
)


def comment_and_body():
    src = SRC.read_text(encoding="utf-8")
    m = re.search(
        r"((?:[ \t]*//[^\n]*\n)+)[ \t]*#\[test\]\s*fn hypothesis_field_set_has_no_metric_surface\(\)\s*\{(.*?)\n    \}\n",
        src,
        re.S,
    )
    if m is None:
        raise AssertionError("hypothesis_field_set_has_no_metric_surface not found")
    comment = "\n".join(l.strip().lstrip("/").strip() for l in m.group(1).splitlines())
    return comment, m.group(2)


class MetricSurfaceCommentMatchesRust(unittest.TestCase):
    def test_literal_names_the_optional_fields(self):
        """Control (not RED-marked): optional fields had to be spelled out."""
        _, body = comment_and_body()
        self.assertIn("scope_write_paths:", body)
        self.assertIn("scope_read_paths:", body)

    @unittest.expectedFailure  # backlog 9e0c9106: open defect, remove when fixed
    def test_comment_does_not_claim_serde_default_keeps_the_literal_compiling(self):
        comment, _ = comment_and_body()
        self.assertIsNone(
            FALSE_CLAIM.search(" ".join(comment.split())),
            "the comment claims a #[serde(default)] field keeps the struct literal "
            "compiling; a struct literal must name every field",
        )


if __name__ == "__main__":
    unittest.main()
