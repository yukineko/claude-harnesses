#!/usr/bin/env python3
"""RED repro for backlog 5e5cf0a9.

5e5cf0a9: the condukt SKILL.md tells the orchestrator to implement `serial`
tasks directly in the MAIN working tree ("serial タスクは worktree に出さず main
で順に実装する", committed via `condukt repo commit`), while CLAUDE.md section 8
names exactly that route and forbids it ("single_worktree モードや serial タスクを
「main で直接実装する」経路として使わない"), and pre-commit's
check-worktree-isolation.py blocks the non-merge main-tree commit it produces.
Any decomposition with file overlap (every plugin task that bumps the shared
marketplace.json) is demoted to serial, so the documented route cannot finish.

Pinned here (contract = the two documents do not contradict each other):
  * control: CLAUDE.md still carries the prohibition (otherwise there is no
    contradiction to test, and the RED test passes vacuously by design);
  * defect (RED): while that prohibition stands, no SKILL.md line routes
    serial-task implementation to the main tree.
"""

from __future__ import annotations

import os
import re
import unittest

SCRIPTS = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(SCRIPTS)
SKILL = os.path.join(REPO, "crates", "condukt", "skills", "condukt", "SKILL.md")
CLAUDE_MD = os.path.join(REPO, "CLAUDE.md")

PROHIBITION = "`serial` タスクを「main で直接実装する」経路として使わない"
# A line about serial tasks that sends their implementation to main.
ROUTE = re.compile(r"serial.*main\s*で(?:順に|1件ずつ|直接)?\s*実装")


def read(p: str) -> str:
    with open(p, encoding="utf-8") as f:
        return f.read()


def prohibition_stands() -> bool:
    return re.sub(r"\s+", "", PROHIBITION) in re.sub(r"\s+", "", read(CLAUDE_MD))


def routing_lines() -> list[str]:
    return [
        f"SKILL.md:{i}: {line.strip()[:120]}"
        for i, line in enumerate(read(SKILL).split("\n"), 1)
        if ROUTE.search(line)
    ]


class SerialRouteMatchesSection8(unittest.TestCase):
    def test_claude_md_prohibits_main_tree_serial_route(self) -> None:
        self.assertTrue(prohibition_stands(), "CLAUDE.md section 8 prohibition text not found")

    @unittest.expectedFailure  # backlog 5e5cf0a9: open defect
    def test_skill_does_not_route_serial_tasks_to_main(self) -> None:
        if not prohibition_stands():
            return
        hits = routing_lines()
        self.assertEqual(
            hits,
            [],
            f"{len(hits)} SKILL.md line(s) route serial tasks to the main tree, which "
            "CLAUDE.md section 8 forbids:\n" + "\n".join(hits),
        )


if __name__ == "__main__":
    unittest.main()
