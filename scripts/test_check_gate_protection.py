#!/usr/bin/env python3
"""RED tests for backlog 3a8e3b73 slice 1: scripts/check-gate-protection.py.

Independent author (CLAUDE.md 2(a)). Contract under test (coordinator ruling):
  exit 0  every non-PENDING listed gate has a complete declaration
  exit 1  empty field / gate neither declared nor PENDING / PENDING grew vs HEAD
          / BLOCKING_GATES not a superset of GATE_CRATES
  exit 2  cannot read/parse
Fixture conventions the tests PIN (the ruling left them open; a later ruling
may change them): `--repo PATH`; list in crates/harness-core/src/fleet.rs as
`pub const BLOCKING_GATES: &[&str] = &[..];`; a declaration is a
`Protection { protects: "..", against: "..", grounds: ".." }` literal in any
.rs under crates/<gate>/src/; baseline scripts/check-gate-protection.baseline,
one gate per line, `#` comments, compared with HEAD's copy.
Deferred to a later slice: refusal one-line summary, taxonomy column.
"""
import subprocess
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
SCRIPT = REPO / "scripts" / "check-gate-protection.py"
BASELINE = "scripts/check-gate-protection.baseline"
GATES = ["alpha", "beta"]


def decl(protects="the asset", against="the adversary", grounds="observed"):
    return (
        "pub const P: Protection = Protection {\n"
        f'    protects: "{protects}",\n    against: "{against}",\n'
        f'    grounds: "{grounds}",\n}};\n'
    )


def git(root, *a):
    subprocess.run(
        ["git", "-C", str(root), "-c", "user.email=t@t", "-c", "user.name=t", *a],
        check=True,
        capture_output=True,
    )


def make_repo(root, gates=GATES, gate_crates=GATES, decls=None, baseline=""):
    """decls: {gate: rust-source or None}. Baseline is committed at HEAD."""
    root = Path(root)
    decls = decls if decls is not None else {g: decl() for g in gates}
    fleet = root / "crates/harness-core/src/fleet.rs"
    fleet.parent.mkdir(parents=True)

    def q(xs):
        return ", ".join(f'"{x}"' for x in xs)

    fleet.write_text(
        f"pub const GATE_CRATES: &[&str] = &[{q(gate_crates)}];\n"
        f"pub const BLOCKING_GATES: &[&str] = &[{q(gates)}];\n"
    )
    for g, src in decls.items():
        if src is not None:
            p = root / f"crates/{g}/src/protection.rs"
            p.parent.mkdir(parents=True)
            p.write_text(src)
    b = root / BASELINE
    b.parent.mkdir(parents=True, exist_ok=True)
    b.write_text(baseline)
    git(root, "init", "-q")
    git(root, "add", "-A")
    git(root, "commit", "-qm", "base")
    return root


def run(root):
    # A missing script makes python exit 2, which would fake the
    # "undetermined" tests; fail loudly instead.
    assert SCRIPT.is_file(), f"{SCRIPT} does not exist (RED: checker not implemented)"
    return subprocess.run(
        ["python3", str(SCRIPT), "--repo", str(root)], capture_output=True, text=True
    )


class Checker(unittest.TestCase):
    def setUp(self):
        self._t = tempfile.TemporaryDirectory()
        self.addCleanup(self._t.cleanup)
        self.root = Path(self._t.name)

    def check(self, want, **kw):
        make_repo(self.root, **kw)
        r = run(self.root)
        self.assertEqual(r.returncode, want, f"stdout={r.stdout!r} stderr={r.stderr!r}")

    # controls (must stay green after the fix)
    def test_complete_declarations_pass(self):
        self.check(0)

    def test_pending_gate_at_baseline_passes(self):
        self.check(0, decls={"alpha": decl(), "beta": None}, baseline="beta\n")

    def test_shrunk_baseline_passes(self):
        make_repo(self.root, baseline="beta\n")  # HEAD lists beta as pending
        (self.root / BASELINE).write_text("")  # beta declared; entry removed
        self.assertEqual(run(self.root).returncode, 0)

    def test_real_repo_passes(self):
        assert SCRIPT.is_file(), "checker not implemented"
        r = subprocess.run(
            ["python3", str(SCRIPT)], capture_output=True, text=True, cwd=REPO
        )
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

    # violations
    def test_empty_protects_fails(self):
        self.check(1, decls={"alpha": decl(protects=""), "beta": decl()})

    def test_empty_against_fails(self):
        self.check(1, decls={"alpha": decl(against="  "), "beta": decl()})

    def test_empty_grounds_fails(self):
        self.check(1, decls={"alpha": decl(), "beta": decl(grounds="")})

    def test_gate_neither_declared_nor_pending_fails(self):
        self.check(1, decls={"alpha": decl(), "beta": None}, baseline="")

    def test_pending_growth_vs_head_fails(self):
        make_repo(self.root, decls={"alpha": decl(), "beta": None}, baseline="beta\n")
        (self.root / BASELINE).write_text("beta\nalpha\n")  # alpha added to PENDING
        (self.root / "crates/alpha/src/protection.rs").unlink()
        self.assertEqual(run(self.root).returncode, 1)

    def test_blocking_gates_not_superset_of_gate_crates_fails(self):
        self.check(
            1, gates=["alpha"], gate_crates=["alpha", "beta"], decls={"alpha": decl()}
        )

    # undetermined
    def test_missing_blocking_gates_const_is_undetermined(self):
        make_repo(self.root)
        (self.root / "crates/harness-core/src/fleet.rs").write_text("// nothing\n")
        self.assertEqual(run(self.root).returncode, 2)

    def test_missing_baseline_is_undetermined(self):
        make_repo(self.root)
        (self.root / BASELINE).unlink()
        self.assertEqual(run(self.root).returncode, 2)

    def test_unparseable_declaration_is_undetermined(self):
        self.check(
            2, decls={"alpha": 'Protection { protects: "x", against: ', "beta": decl()}
        )


if __name__ == "__main__":
    unittest.main()
