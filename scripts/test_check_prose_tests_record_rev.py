#!/usr/bin/env python3
"""check-prose-tests.py must accept the rev-pinned record opener
`<!-- record: YYYY-MM-DD rev: <sha> -->` (user ruling 2026-10-15).

Run: python3 -m pytest -q scripts/test_check_prose_tests_record_rev.py
"""
import pathlib
import subprocess
import sys

SCRIPT = pathlib.Path(__file__).resolve().parent / "check-prose-tests.py"


def git(repo, *a):
    return subprocess.run(["git", "-C", str(repo), *a], check=True,
                          capture_output=True, text=True).stdout.strip()


def test_e_rev_pinned_record_opener_is_a_record(tmp_path):
    repo = tmp_path / "repo"
    (repo / "docs").mkdir(parents=True)
    git(repo, "init", "-q")
    git(repo, "config", "user.name", "t")
    git(repo, "config", "user.email", "t@e.com")
    git(repo, "config", "commit.gpgsign", "false")
    (repo / "docs/d.md").write_text("x\n")
    git(repo, "add", "-A")
    git(repo, "commit", "-q", "-m", "c1")
    rev = git(repo, "rev-parse", "HEAD")
    (repo / "docs/d.md").write_text(
        f"<!-- record: 2026-10-01 rev: {rev} -->\n"
        "The gate blocked unsigned commits.\n"
        "<!-- /record -->\n")
    r = subprocess.run([sys.executable, str(SCRIPT), "--repo", str(repo), "--all"],
                       capture_output=True, text=True, timeout=60)
    assert "malformed-record-block" not in r.stdout, (r.stdout, r.stderr)
    assert r.returncode == 0, (r.returncode, r.stdout, r.stderr)
