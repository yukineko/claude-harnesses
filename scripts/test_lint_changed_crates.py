#!/usr/bin/env python3
"""Acceptance tests for `scripts/lint-changed-crates.sh`.

Stdlib-only (`unittest`), same shape as `scripts/test_gate_bypass.py`: every
assertion is made against a THROWAWAY `git init` cargo workspace built in a temp
directory.  Nothing here runs against the real repository, and nothing here
writes to it.

    python3 scripts/test_lint_changed_crates.py

WHAT IS UNDER TEST.  donegate's `fmt` and `clippy` checks used to run
`cargo fmt --all` / `cargo clippy --workspace`, so a red crate that the current
turn never touched blocked the stop of whoever *was* working — the attribution
bug CLAUDE.md #8 describes (a worktree/session gets blocked by someone else's
red).  `lint-changed-crates.sh` scopes both to the crates changed in the working
tree.  The property, in both directions:

  UNTOUCHED crate is red  -> the script still exits 0  (the ticket's criterion)
  TOUCHED   crate is red  -> the script exits non-zero (the anti-vacuity control)

The second class is not decoration.  Without it, a script that does nothing at
all — or one that cannot find cargo and silently skips — passes the first class
perfectly.  Each "does not block" test therefore also asserts, in the SAME
scratch repo, that the workspace-wide command the gate used to run (`cargo fmt
--all -- --check`) really is non-zero there: otherwise "exit 0" would only prove
that the planted violation was not a violation.

The second class of tests covers the fail-CLOSED behaviour carried over from
`scripts/test-changed-crates.sh`: cannot-determine (absent cargo, failing git)
must never be reported as "nothing to lint".

A third class reads the repository's real `donegate.toml` and pins HOW the
script is wired in (one check, 600s, the union of the old triggers, and no
workspace-wide lint anywhere) — the configuration half of the same property.

Point the suite at a MUTATED script — or a MUTATED donegate config — to observe
it going RED:

    LINT_CHANGED_CRATES_SCRIPT=/tmp/mutant/lint-changed-crates.sh \\
        python3 scripts/test_lint_changed_crates.py
    DONEGATE_TOML_UNDER_TEST=/tmp/mutant/donegate.toml \\
        python3 scripts/test_lint_changed_crates.py DonegateWiresItAsOneCheck
"""

from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
import tomllib
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_REPO = _HERE.parent

# The script under test.  Overridable so a mutant can be pointed at this suite
# (see the module docstring) — the only way to show these tests are not vacuous.
_SCRIPT = Path(
    os.environ.get("LINT_CHANGED_CRATES_SCRIPT", _HERE / "lint-changed-crates.sh")
).resolve()

# Copied into every scratch repo because the script calls it by path before it
# builds anything.  Real file, real behaviour (it no-ops well under its cap).
_CAP_SCRIPT = _HERE / "cap-target-dir.sh"

# The donegate config whose WIRING is pinned below.  Overridable for the same
# reason the script is: a regression pin that has never been observed failing
# proves nothing, and the only way to observe this one failing is to point it at
# a config that has the defect.
_DONEGATE_TOML = Path(
    os.environ.get("DONEGATE_TOML_UNDER_TEST", _REPO / "donegate.toml")
)


def _which(name: str) -> str:
    path = shutil.which(name)
    if path is None:  # pragma: no cover - environment defect
        raise unittest.SkipTest("required tool not on PATH: " + name)
    return path


def _require_cargo_toolchain() -> None:
    """Both cargo subcomponents this script drives must exist, or the suite is
    measuring the environment instead of the script.  Skipping is honest here
    (nothing was observed); a silent pass would not be."""
    cargo = shutil.which("cargo")
    if cargo is None:
        raise unittest.SkipTest("cargo is not on PATH")
    for sub in ("fmt", "clippy"):
        proc = subprocess.run(
            [cargo, sub, "--version"], capture_output=True, text=True
        )
        if proc.returncode != 0:
            raise unittest.SkipTest(
                "cargo %s is unavailable: %s" % (sub, proc.stderr.strip())
            )


# ── scratch workspace contents ───────────────────────────────────────────────

ROOT_MANIFEST = """\
[workspace]
resolver = "2"
members = ["crates/alpha", "crates/beta"]
"""

# `[lib]` deliberately precedes `[package]` and carries a DIFFERENT `name`:
# a naive "first name = line wins" reader would resolve the package as
# `not_the_package_name` and `cargo fmt -p` would fail.  The script resolves the
# name from the `[package]` section only, and this manifest is what proves it.
ALPHA_MANIFEST = """\
[lib]
name = "not_the_package_name"
path = "src/lib.rs"

[package]
name = "scratch-alpha"
version = "0.0.0"
edition = "2021"
"""

BETA_MANIFEST = """\
[package]
name = "scratch-beta"
version = "0.0.0"
edition = "2021"

[lib]
path = "src/lib.rs"
"""

CLEAN_LIB = """\
pub fn answer() -> u32 {
    42
}
"""

# rustfmt-clean addition, so "crate A was modified" does not by itself make the
# fmt check red.
CLEAN_ADDITION = """\
pub fn answer() -> u32 {
    42
}

pub fn also_clean(x: u32) -> u32 {
    x + 1
}
"""

# `cargo fmt -- --check` reports a diff for this and exits non-zero.
FMT_VIOLATION = """\
pub fn answer() -> u32 {
    42
}

pub fn badly_formatted(   ) ->u32{let x=1;x}
"""

# rustfmt-clean, but `clippy::needless_return` fires, and the gate runs clippy
# with `-D warnings`.
CLIPPY_VIOLATION = """\
pub fn answer() -> u32 {
    42
}

pub fn needless(x: u32) -> u32 {
    return x;
}
"""


class Workspace:
    """A throwaway git repo holding a two-crate cargo workspace and a copy of
    the script under test."""

    def __init__(self, seed: bool = True) -> None:
        # .resolve() so paths match what git reports on macOS (/var ->
        # /private/var).
        self.root = Path(tempfile.mkdtemp(prefix="lint-changed-test-")).resolve()

        # Inherit the real PATH/HOME on purpose: cargo here is rustup-managed and
        # a throwaway HOME would leave the shim with no toolchain to run, so every
        # cargo-backed assertion would fail for a reason unrelated to the script.
        self.env = dict(os.environ)
        for stale in ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"):
            self.env.pop(stale, None)
        self.env.update(
            LC_ALL="C",
            GIT_CONFIG_NOSYSTEM="1",
            GIT_CONFIG_GLOBAL=os.devnull,
            GIT_TERMINAL_PROMPT="0",
            GIT_CEILING_DIRECTORIES=str(self.root.parent),
            GIT_AUTHOR_NAME="t",
            GIT_AUTHOR_EMAIL="t@example.invalid",
            GIT_COMMITTER_NAME="t",
            GIT_COMMITTER_EMAIL="t@example.invalid",
        )

        subprocess.run(
            [_which("git"), "init", "-q", "-b", "main", str(self.root)],
            env=self.env, capture_output=True, text=True, check=True,
        )
        self.git("config", "commit.gpgsign", "false")

        self.script_dir = self.root / "scripts"
        self.script_dir.mkdir()
        self.script = self.script_dir / "lint-changed-crates.sh"
        if _SCRIPT.exists():
            shutil.copy2(_SCRIPT, self.script)
            self.script.chmod(0o755)
        if _CAP_SCRIPT.exists():
            shutil.copy2(_CAP_SCRIPT, self.script_dir / "cap-target-dir.sh")
            (self.script_dir / "cap-target-dir.sh").chmod(0o755)

        if seed:
            self.seed()

    # --- construction helpers -------------------------------------------
    def seed(self) -> None:
        self.write(".gitignore", "/target\n")
        self.write("Cargo.toml", ROOT_MANIFEST)
        self.write("crates/alpha/Cargo.toml", ALPHA_MANIFEST)
        self.write("crates/alpha/src/lib.rs", CLEAN_LIB)
        self.write("crates/beta/Cargo.toml", BETA_MANIFEST)
        self.write("crates/beta/src/lib.rs", CLEAN_LIB)
        self.git("add", "-A")
        self.git("commit", "-q", "-m", "seed")

    def write(self, relpath: str, text: str) -> None:
        target = self.root / relpath
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)

    def commit_all(self, message: str) -> None:
        self.git("add", "-A")
        self.git("commit", "-q", "-m", message)

    def cleanup(self) -> None:
        shutil.rmtree(self.root, ignore_errors=True)

    # --- running --------------------------------------------------------
    def git(self, *args: str, check: bool = True) -> subprocess.CompletedProcess:
        proc = subprocess.run(
            [_which("git")] + list(args),
            cwd=str(self.root), env=self.env, capture_output=True, text=True,
        )
        if check and proc.returncode != 0:
            raise AssertionError(
                "git %s failed (%d)\n--- stdout ---\n%s\n--- stderr ---\n%s"
                % (" ".join(args), proc.returncode, proc.stdout, proc.stderr)
            )
        return proc

    def run_script(self, env_extra: dict | None = None,
                   cwd: Path | None = None) -> subprocess.CompletedProcess:
        env = dict(self.env)
        env.update(env_extra or {})
        return subprocess.run(
            ["bash", str(self.script)],
            cwd=str(cwd or self.root), env=env, capture_output=True, text=True,
            stdin=subprocess.DEVNULL,
        )

    def workspace_wide_fmt(self) -> subprocess.CompletedProcess:
        """What donegate used to run.  Used as a CONTROL: it must be red in the
        repos where the script is expected to pass, or `exit 0` proves nothing."""
        return subprocess.run(
            [_which("cargo"), "fmt", "--all", "--", "--check"],
            cwd=str(self.root), env=self.env, capture_output=True, text=True,
        )


def _detail(proc: subprocess.CompletedProcess) -> str:
    return "\n--- stdout ---\n%s\n--- stderr ---\n%s" % (proc.stdout, proc.stderr)


class ScopedToTheCratesYouTouched(unittest.TestCase):
    """The ticket: a red crate you did not touch must not block your stop."""

    @classmethod
    def setUpClass(cls):
        _require_cargo_toolchain()

    def setUp(self):
        self.ws = Workspace()
        self.addCleanup(self.ws.cleanup)

    # -- fmt ------------------------------------------------------------
    def test_fmt_red_in_an_untouched_crate_does_not_block(self):
        """crate B is committed badly formatted; only crate A is modified."""
        self.ws.write("crates/beta/src/lib.rs", FMT_VIOLATION)
        self.ws.commit_all("beta lands a formatting violation")
        self.ws.write("crates/alpha/src/lib.rs", CLEAN_ADDITION)

        control = self.ws.workspace_wide_fmt()
        self.assertNotEqual(
            0, control.returncode,
            "CONTROL FAILED: `cargo fmt --all -- --check` is green in this "
            "scratch repo, so the planted violation is not one and the "
            "assertion below would prove nothing." + _detail(control),
        )

        proc = self.ws.run_script()
        self.assertEqual(
            0, proc.returncode,
            "a formatting violation in the crate this turn did NOT touch "
            "blocked the stop" + _detail(proc),
        )
        self.assertIn("scratch-alpha", proc.stdout)
        self.assertNotIn("scratch-beta", proc.stdout)

    def test_fmt_red_in_the_crate_you_touched_blocks(self):
        """ANTI-VACUITY CONTROL: same repo, violation moved into crate A."""
        self.ws.write("crates/alpha/src/lib.rs", FMT_VIOLATION)

        proc = self.ws.run_script()
        self.assertNotEqual(
            0, proc.returncode,
            "a formatting violation in the crate this turn DID touch was let "
            "through — the check is vacuous" + _detail(proc),
        )
        self.assertIn("scratch-alpha", proc.stdout + proc.stderr)

    # -- clippy ---------------------------------------------------------
    def test_clippy_red_in_an_untouched_crate_does_not_block(self):
        self.ws.write("crates/beta/src/lib.rs", CLIPPY_VIOLATION)
        self.ws.commit_all("beta lands a clippy violation")
        self.ws.write("crates/alpha/src/lib.rs", CLEAN_ADDITION)

        # CONTROL: beta really is red under the workspace-wide command.
        control = subprocess.run(
            [_which("cargo"), "clippy", "--workspace", "--all-targets",
             "--", "-D", "warnings"],
            cwd=str(self.ws.root), env=self.ws.env, capture_output=True, text=True,
        )
        self.assertNotEqual(
            0, control.returncode,
            "CONTROL FAILED: workspace-wide clippy is green here, so the "
            "planted violation is not one" + _detail(control),
        )

        proc = self.ws.run_script()
        self.assertEqual(
            0, proc.returncode,
            "a clippy violation in the crate this turn did NOT touch blocked "
            "the stop" + _detail(proc),
        )

    def test_clippy_red_in_the_crate_you_touched_blocks(self):
        """ANTI-VACUITY CONTROL for the clippy half."""
        self.ws.write("crates/alpha/src/lib.rs", CLIPPY_VIOLATION)

        proc = self.ws.run_script()
        self.assertNotEqual(
            0, proc.returncode,
            "a clippy violation in the crate this turn DID touch was let "
            "through — the check is vacuous" + _detail(proc),
        )
        # Non-zero alone would also be satisfied by a script that does not
        # exist (rc 127), so pin that the touched package was actually linted.
        self.assertIn("scratch-alpha", proc.stdout + proc.stderr, _detail(proc))


class FailsClosedWhenItCannotDetermine(unittest.TestCase):
    """Cannot-determine must never be reported as 'nothing to lint'
    (CLAUDE.md #3).  Carried over from scripts/test-changed-crates.sh."""

    def setUp(self):
        self.ws = Workspace()
        self.addCleanup(self.ws.cleanup)

    def test_absent_cargo_is_not_a_skip(self):
        # A PATH with git but no cargo, and a HOME with no .cargo/env to
        # recover it from.
        fake_home = self.ws.root / ".fake-home"
        fake_home.mkdir()
        proc = self.ws.run_script(env_extra={
            "PATH": "/usr/bin:/bin",
            "HOME": str(fake_home),
        })
        self.assertEqual(1, proc.returncode,
                         "absent cargo must fail closed" + _detail(proc))
        self.assertIn("cargo not found", proc.stderr)
        self.assertNotIn("nothing to lint", proc.stdout)

    def test_a_failing_git_diff_is_not_reported_as_nothing_to_lint(self):
        stub_dir = self.ws.root.parent / (self.ws.root.name + "-stubbin")
        stub_dir.mkdir()
        real_git = _which("git")
        stub = stub_dir / "git"
        stub.write_text(
            "#!/usr/bin/env bash\n"
            'if [ "$1" = "diff" ]; then\n'
            '  echo "fatal: simulated git diff failure" >&2\n'
            "  exit 128\n"
            "fi\n"
            'exec %s "$@"\n' % real_git
        )
        stub.chmod(0o755)
        self.addCleanup(shutil.rmtree, str(stub_dir), True)

        proc = self.ws.run_script(env_extra={
            "PATH": str(stub_dir) + os.pathsep + self.ws.env["PATH"],
        })
        self.assertEqual(1, proc.returncode,
                         "a git-diff failure must fail closed" + _detail(proc))
        self.assertIn("could not determine the changed file set", proc.stderr)
        self.assertNotIn("no crate touched", proc.stdout)

    def test_an_unborn_branch_is_determinable_not_fail_closed(self):
        ws = Workspace(seed=False)
        self.addCleanup(ws.cleanup)
        ws.write("crates/skillonly/SKILL.md", "# skill\n")
        ws.git("add", "crates/skillonly/SKILL.md")  # staged: visible to ls-files

        proc = ws.run_script()
        self.assertEqual(
            0, proc.returncode,
            "an unborn branch is a determinable state, not a broken one"
            + _detail(proc),
        )
        # A crates/ dir with no Cargo.toml proves the unborn fallback POPULATED
        # the change set; an empty set would have said "no crate touched".
        self.assertIn("no Cargo.toml", proc.stdout, _detail(proc))

    def test_nothing_touched_costs_nothing(self):
        proc = self.ws.run_script()
        self.assertEqual(0, proc.returncode, _detail(proc))
        self.assertIn("no crate touched", proc.stdout)

    def test_outside_a_git_repo_fails_closed(self):
        outside = Path(tempfile.mkdtemp(prefix="lint-changed-nogit-")).resolve()
        self.addCleanup(shutil.rmtree, str(outside), True)
        proc = self.ws.run_script(cwd=outside)
        self.assertEqual(
            1, proc.returncode,
            "no repo means no changed set can be determined — fail closed"
            + _detail(proc),
        )
        self.assertIn("not a git repo", proc.stderr)


class DonegateWiresItAsOneCheck(unittest.TestCase):
    """Pins HOW donegate.toml calls the script — the repository's real file, not
    a scratch copy.

    WHY THIS IS PINNED AT ALL.  The script was first wired in as the `cmd` of
    BOTH pre-existing checks (`fmt` and `clippy`).  Since one invocation runs
    fmt AND clippy, that made every Stop run the whole sweep TWICE, and left the
    `fmt` entry budgeting 120s for a job that now includes a cold clippy build —
    donegate would report `fmt` as a TIMEOUT while `clippy` (600s) passed.  The
    fix is one check at the binding (600s) budget.  Without this test the
    collapse is unpinned and the next editor re-splits it; the failure mode is a
    spurious block, which is exactly what gets a gate disabled.

    Which half went red is still reported — by the script's own stderr
    (`cargo fmt --check FAILED for ...` / `cargo clippy FAILED for ...`), which
    is what the two check NAMES used to buy."""

    UNION_TRIGGERS = {"**/*.rs", "Cargo.toml", "Cargo.lock"}

    def setUp(self):
        manifest = _DONEGATE_TOML
        # No donegate.toml means nothing can be pinned about the wiring. That is
        # a failure of this test, not a pass by absence.
        self.assertTrue(manifest.is_file(), "%s does not exist" % manifest)
        self.config = tomllib.loads(manifest.read_text())
        self.checks = self.config.get("check", [])
        self.assertTrue(self.checks, "donegate.toml declares no [[check]] at all")

    def _lint_checks(self):
        return [c for c in self.checks if "lint-changed-crates.sh" in c.get("cmd", "")]

    def test_the_lint_script_is_wired_in_exactly_once(self):
        lint = self._lint_checks()
        self.assertEqual(
            1, len(lint),
            "one invocation of lint-changed-crates.sh already runs BOTH fmt and "
            "clippy, so N entries mean N full sweeps per Stop; found %d: %r"
            % (len(lint), [c.get("name") for c in lint]),
        )
        self.assertEqual("lint-changed", lint[0].get("name"))

    def test_it_carries_the_binding_budget_and_the_union_of_the_triggers(self):
        lint = self._lint_checks()
        self.assertEqual(1, len(lint), "precondition: exactly one lint check")
        check = lint[0]
        self.assertEqual(
            600, check.get("timeout_secs"),
            "the budget has to cover the clippy half (the old 120s fmt budget "
            "would fire a spurious TIMEOUT on a cold clippy build)",
        )
        self.assertEqual(
            self.UNION_TRIGGERS, set(check.get("when_changed", [])),
            "the single check must still fire for everything the two old checks "
            "fired for — a Cargo.toml/Cargo.lock-only change must reach clippy",
        )

    def test_no_check_lints_the_whole_workspace_any_more(self):
        """The ticket's criterion, at the configuration layer: nothing in
        donegate.toml may run the workspace-wide form again."""
        offenders = [
            (c.get("name"), c.get("cmd"))
            for c in self.checks
            if "cargo fmt --all" in c.get("cmd", "")
            or "cargo clippy --workspace" in c.get("cmd", "")
        ]
        self.assertEqual(
            [], offenders,
            "a workspace-wide lint is back in donegate.toml, so a crate this "
            "turn never touched can block this turn's stop again: %r" % offenders,
        )


if __name__ == "__main__":
    unittest.main(verbosity=2)
