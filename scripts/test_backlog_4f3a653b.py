#!/usr/bin/env python3
"""Repro test for backlog 4f3a653b (CA-config-parse-001): a config file that does
not parse is silently replaced by defaults across many crates.

The shape: `if let Ok(fc) = toml::from_str::<FileConfig>(&text) { ...apply... }`
with NO else arm, or `toml::from_str(..).ok()` / `.unwrap_or_default()`. A config
file that exists but cannot be parsed then behaves exactly like "no config" —
"cannot determine" mapped to "defaults are fine" (CLAUDE.md 3), invisibly.

The test scans non-test Rust under crates/*/src for those erasures and requires
there be none (either per-crate handling or a shared Determination loader — the
item's open design choice — would clear it). Open; expectedFailure. The control
proves the scanner detects the shape on a synthetic snippet and does NOT flag an
`if let Ok` that has an else arm.
"""

import re
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

IF_LET = re.compile(r"if let Ok\([^)]*\)\s*=\s*toml::from_str\b")
DIRECT = re.compile(r"toml::from_str\b[^;]*?\)\s*\.\s*(ok\(\)|unwrap_or_default\(\)|unwrap_or\()", re.S)


def _strip_tests(src):
    # Drop `#[cfg(test)] mod ... { ... }` blocks (brace-matched).
    out, i = [], 0
    for m in re.finditer(r"#\[cfg\(test\)\]\s*(?:pub\s+)?mod\s+\w+\s*\{", src):
        if m.start() < i:
            continue
        out.append(src[i:m.start()])
        depth, j = 1, m.end()
        while j < len(src) and depth:
            depth += {"{": 1, "}": -1}.get(src[j], 0)
            j += 1
        i = j
    out.append(src[i:])
    return "".join(out)


def _block_end(src, open_idx):
    depth, j = 0, open_idx
    while j < len(src):
        c = src[j]
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                return j + 1
        j += 1
    return len(src)


def findings_in(src, label="<src>"):
    src = _strip_tests(src)
    hits = []
    for m in IF_LET.finditer(src):
        brace = src.find("{", m.end())
        end = _block_end(src, brace)
        if not re.match(r"\s*else\b", src[end:]):
            hits.append("%s:%d if-let-Ok without else" % (label, src.count("\n", 0, m.start()) + 1))
    for m in DIRECT.finditer(src):
        hits.append("%s:%d .%s" % (label, src.count("\n", 0, m.start()) + 1, m.group(1)))
    return hits


def scan():
    hits = []
    for p in sorted(REPO.glob("crates/*/src/**/*.rs")):
        if p.name.endswith("_tests.rs") or "/tests/" in str(p):
            continue
        text = p.read_text()
        if "toml::from_str" in text:
            hits += findings_in(text, str(p.relative_to(REPO)))
    return hits


class ConfigParseErasure(unittest.TestCase):
    def test_control_scanner_shape(self):
        bad = "fn f(t:&str){ if let Ok(fc) = toml::from_str::<C>(t) { use_it(fc); } }"
        good = "fn f(t:&str){ if let Ok(fc) = toml::from_str::<C>(t) { use_it(fc); } else { warn(); } }"
        direct = "fn f(t:&str)->C{ toml::from_str(t).unwrap_or_default() }"
        self.assertEqual(len(findings_in(bad)), 1)
        self.assertEqual(findings_in(good), [])
        self.assertEqual(len(findings_in(direct)), 1)

    @unittest.expectedFailure
    def test_no_config_parse_error_is_erased(self):
        hits = scan()
        self.assertEqual(hits, [], "%d erased toml parse errors:\n%s" % (len(hits), "\n".join(hits)))


if __name__ == "__main__":
    unittest.main()
