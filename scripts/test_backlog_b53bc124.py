#!/usr/bin/env python3
"""backlog b53bc124: flow's 3-3 sink closes the backlog item and releases the
claim, but never closes the condukt RUN-STATE. A worktree-less deploy task
(rollout) therefore stays `running` forever: `condukt state probe` is
undetermined for it ("task has no worktree"), `reconcile` has no branch to
look at, and autoflow's Stop hook keeps reporting it as a pending task.

The item's own discriminator: does the 3-3 sink section of
crates/flow/skills/flow/SKILL.md mention `condukt state set` or a
`condukt gate` step? If not, reading 1 (real defect) holds.

Run:  python3 -m unittest scripts.test_backlog_b53bc124
"""
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
SKILL = REPO / "crates" / "flow" / "skills" / "flow" / "SKILL.md"


def sink_section():
    text = SKILL.read_text()
    start = text.index("#### 3-3.")
    end = text.index("#### 3-4.", start)
    return text[start:end]


class FlowSinkClosesRunState(unittest.TestCase):
    def test_precondition_sink_section_exists_and_releases_claim(self):
        s = sink_section()
        self.assertIn("backlog done", s)
        self.assertIn("condukt state release-task", s)

    @unittest.expectedFailure
    def test_sink_closes_the_condukt_run_state(self):
        """backlog b53bc124: open defect (RED observed)."""
        s = sink_section()
        self.assertTrue(
            "condukt state set" in s or "condukt gate" in s,
            "flow SKILL.md 3-3 sink has no step that closes the condukt run-state "
            "(no `condukt state set` / `condukt gate`)")


if __name__ == "__main__":
    unittest.main()
