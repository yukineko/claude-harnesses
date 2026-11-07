"""Shared fixture for audit-B pre-commit ratchet REDs (items 9 and 20).

Not a test suite itself (no `test_` prefix). It answers one question without
naming the scanner the fix will add: "does ANY scanner wired into
`.githooks/pre-commit` (a `run <scanner>.py <label>` line) refuse this staged
diff, while accepting the same diff with the offending construct replaced by a
live one?"

The differential is what makes the answer about the construct: a scanner that
blocks the control too (for example bump-on-change, because the fixture touches
a crate without a version bump, or a scanner that cannot work in a minimal
fixture) is excluded, so it cannot satisfy the property by accident.

Fixture: a throwaway git repo with a copy of this repo's `scripts/` and
`.githooks/` (so every scanner's `Path(__file__)` / cwd resolves to the
fixture, never to the real repo), a one-crate workspace, a base commit, and
the case's change STAGED on top.
"""
import os
import re
import shutil
import subprocess
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
PRE_COMMIT = REPO / ".githooks" / "pre-commit"


def wired_scanners():
    """The `run <x>.py` scanners .githooks/pre-commit executes, in order."""
    text = PRE_COMMIT.read_text(encoding="utf-8")
    return re.findall(r"^run\s+(\S+\.py)\s", text, flags=re.M)


def _git(cwd, *args):
    subprocess.run(
        ["git", *args],
        cwd=cwd,
        check=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )


def _build(tmp, crate, rel_src, base_src, staged_src):
    root = Path(tmp)
    shutil.copytree(REPO / "scripts", root / "scripts", ignore=shutil.ignore_patterns("__pycache__"))
    shutil.copytree(REPO / ".githooks", root / ".githooks")
    (root / "Cargo.toml").write_text(
        f'[workspace]\nmembers = ["crates/{crate}"]\nresolver = "2"\n', encoding="utf-8"
    )
    cdir = root / "crates" / crate
    (cdir / "src").mkdir(parents=True)
    (cdir / "Cargo.toml").write_text(
        f'[package]\nname = "{crate}"\nversion = "0.1.0"\nedition = "2021"\n', encoding="utf-8"
    )
    target = cdir / rel_src
    target.write_text(base_src, encoding="utf-8")
    env_git = ("-c", "user.name=fixture", "-c", "user.email=fixture@example.invalid")
    _git(root, "init", "-q", "-b", "main")
    _git(root, "add", "-A")
    _git(root, *env_git, "commit", "-q", "--no-verify", "-m", "base")
    target.write_text(staged_src, encoding="utf-8")
    _git(root, "add", "-A")
    return root


def run_scanners(crate, rel_src, base_src, staged_src, only=None):
    """{scanner: (exit code, output tail)} for every wired scanner (or those in
    `only`) run against the fixture."""
    with tempfile.TemporaryDirectory() as tmp:
        root = _build(tmp, crate, rel_src, base_src, staged_src)
        env = dict(os.environ)
        for k in list(env):
            if k.startswith("GIT_"):
                del env[k]
        out = {}
        for s in wired_scanners():
            if only is not None and s not in only:
                continue
            p = subprocess.run(
                ["python3", str(root / "scripts" / s)],
                cwd=root,
                env=env,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
                timeout=300,
            )
            out[s] = (p.returncode, p.stdout[-400:])
        return out


def accepting(results):
    """Scanners that exited 0."""
    return sorted(s for s, (rc, _) in results.items() if rc == 0)


def refusing_scanners(crate, rel_src, base_src, bad_src, control):
    """Scanners that accepted `control` (a run_scanners result for the live
    variant of the same diff) but refuse `bad_src`; plus the bad results."""
    ok = accepting(control)
    bad = run_scanners(crate, rel_src, base_src, bad_src, only=set(ok))
    return [s for s in ok if bad[s][0] != 0], bad
