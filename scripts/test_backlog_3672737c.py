"""Repro for backlog 3672737c: after rust-toolchain.toml pinned 1.97.1 (with no `targets`
key), the pinned toolchain has no cross targets, so
`scripts/build-plugin-bin.sh <plugin> x86_64-apple-darwin` (its own documented example)
and the linux cross build cannot work until someone runs `rustup target add` by hand.

This asks rustup, from the repo root (so the rust-toolchain.toml override applies),
which targets the pinned toolchain has. A `targets = [...]` entry in rust-toolchain.toml
makes rustup install them on first use, which is what turns this GREEN. "wasm32" is
named in the item without a triple, so it is not asserted here.
"""
import os
import shutil
import subprocess
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
CROSS_TARGETS = ("x86_64-apple-darwin", "x86_64-unknown-linux-gnu")


def rustup():
    found = shutil.which("rustup") or os.path.expanduser("~/.cargo/bin/rustup")
    if not os.path.exists(found):
        raise AssertionError("rustup not found: cannot determine the installed targets")
    return found


def installed_targets():
    r = subprocess.run([rustup(), "target", "list", "--installed"], cwd=REPO,
                       capture_output=True, text=True, timeout=300)
    if r.returncode != 0:
        raise AssertionError(f"rustup target list failed rc={r.returncode}: {r.stderr[-400:]}")
    return set(r.stdout.split())


class Backlog3672737c(unittest.TestCase):
    def test_control_toolchain_is_pinned_and_host_target_present(self):
        self.assertIn('channel = "1.97.1"', (REPO / "rust-toolchain.toml").read_text())
        self.assertTrue(installed_targets(), "rustup reported no installed target at all")

    @unittest.expectedFailure  # backlog 3672737c: open defect, remove when fixed
    def test_pinned_toolchain_has_the_cross_targets(self):
        missing = [t for t in CROSS_TARGETS if t not in installed_targets()]
        self.assertEqual(missing, [], "pinned toolchain lacks cross targets")


if __name__ == "__main__":
    unittest.main()
