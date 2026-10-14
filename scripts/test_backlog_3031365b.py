#!/usr/bin/env python3
"""backlog 3031365b: duplicate of 0e4cfde5 — the .githooks/pre-commit
main-tree-guard does not find the deployed plugin binary
(~/.claude/plugins/cache/yukineko/condukt/<ver>/bin/condukt-darwin-arm64), so a
commit from a condukt worktree without CONDUKT_BIN is always refused
("no condukt binary found"). Same fixture and assertions as
scripts/test_backlog_0e4cfde5.py.

RED repro:  python3 -m pytest --runxfail scripts/test_backlog_3031365b.py
"""
from __future__ import annotations

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_backlog_0e4cfde5 import PluginCacheCondukt  # noqa: E402,F401

if __name__ == "__main__":
    unittest.main()
