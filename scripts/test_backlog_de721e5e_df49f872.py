"""Repros for two gates that exist as scripts but are wired to nothing.

de721e5e: scripts/mutation-gate.sh / scripts/mutation-pilots.sh (the kill-rate gate)
  had exactly one launcher, .github/workflows/mutation.yml, removed in a572f5ad
  (CLAUDE.md §7 bans GitHub Actions). Nothing under .githooks/ invokes them, so the
  only gate that measures whether tests can detect defects never runs.
df49f872: scripts/check-projkey-store-classification.py is implemented but no hook
  runs it, so an unclassified projkey-store consumer can land without anything going
  red. A second test pins that the gate is not even green today, i.e. wiring it
  will also require repairing its table.

Each defect test asserts the CORRECT state and fails while the item is open.
Stdlib only; read-only except the checker run (which only reads the tree).
"""
import re
import subprocess
import sys
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
HOOKS = REPO / ".githooks"


def hook_texts():
    return {p.name: p.read_text(encoding="utf-8", errors="replace")
            for p in sorted(HOOKS.iterdir()) if p.is_file()}


def invoking_hooks(script_name):
    """Hooks with a non-comment line that names `script_name`."""
    hits = []
    for name, text in hook_texts().items():
        for line in text.splitlines():
            code = line.split("#", 1)[0]
            if re.search(r"(^|[\s/\"'])" + re.escape(script_name) + r"\b", code):
                hits.append(name)
                break
    return hits


class BacklogDe721e5e(unittest.TestCase):
    def test_control_gate_scripts_exist_and_hooks_dir_is_scanned(self):
        self.assertTrue((REPO / "scripts" / "mutation-gate.sh").is_file())
        self.assertTrue((REPO / "scripts" / "mutation-pilots.sh").is_file())
        # sanity: the scanner sees a script that IS wired
        self.assertIn("pre-commit", invoking_hooks("check-fail-open.py"))

    @unittest.expectedFailure  # backlog de721e5e: open defect, remove when fixed
    def test_some_local_hook_launches_the_mutation_gate(self):
        hits = invoking_hooks("mutation-gate.sh") + invoking_hooks("mutation-pilots.sh")
        self.assertNotEqual(hits, [], "no .githooks/* launches mutation-gate.sh / mutation-pilots.sh")


class BacklogDf49f872(unittest.TestCase):
    def test_control_checker_exists(self):
        self.assertTrue((REPO / "scripts" / "check-projkey-store-classification.py").is_file())

    @unittest.expectedFailure  # backlog df49f872: open defect, remove when fixed
    def test_pre_commit_runs_projkey_store_classification(self):
        self.assertIn("pre-commit", invoking_hooks("check-projkey-store-classification.py"))

    @unittest.expectedFailure  # backlog df49f872: the table has drifted; wiring needs it green
    def test_projkey_store_classification_is_green_on_this_tree(self):
        r = subprocess.run([sys.executable, "scripts/check-projkey-store-classification.py"],
                           cwd=REPO, capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, (r.stdout + r.stderr)[-1500:])


if __name__ == "__main__":
    unittest.main()
