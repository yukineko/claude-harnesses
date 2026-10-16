#!/usr/bin/env python3
"""backlog 346b68b2: duplicate of 0e4cfde5 — the .githooks/pre-commit
main-tree-guard does not find the condukt installed in the plugin cache
(~/.claude/plugins/cache/yukineko/condukt/<ver>/bin/), so a worktree commit
without CONDUKT_BIN / target/ is always refused ("no condukt binary found").
Same fixture and assertions as scripts/test_backlog_0e4cfde5.py.

RED repro:  python3 -m pytest --runxfail scripts/test_backlog_346b68b2.py
"""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_backlog_0e4cfde5 import PluginCacheCondukt  # noqa: E402,F401

if __name__ == "__main__":
    unittest.main()
