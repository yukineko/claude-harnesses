#!/usr/bin/env python3
"""Tests for the rev-pinned record block contract of check-doc-claims.py
(user ruling 2026-10-15): `<!-- record: YYYY-MM-DD rev: <rev> -->` ..
`<!-- /record -->`; citations inside are judged against `git show <rev>:<path>`,
a record without a (resolvable) rev is NOT a record and BLOCKS.

Run: python3 -m pytest -q scripts/test_check_doc_claims_record_rev.py
"""
import os
import pathlib
import subprocess
import sys

SCRIPT = pathlib.Path(__file__).resolve().parent / "check-doc-claims.py"
ENV = {
    "LC_ALL": "C",
    "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
    "GIT_CONFIG_GLOBAL": "/dev/null",
    "GIT_CONFIG_SYSTEM": "/dev/null",
    "GIT_CONFIG_NOSYSTEM": "1",
    "GIT_TERMINAL_PROMPT": "0",
    "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@e.com",
    "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@e.com",
}

LONG = "".join(f"// line {i}\n" for i in range(1, 8)) + "const NEEDLE: u8 = 1;\n" + "// line 9\n// line 10\n"  # NEEDLE on line 8
SHORT = "// only\n// three\n// lines\n"


def git(repo, *args):
    p = subprocess.run(["git", "-C", str(repo), *args], capture_output=True,
                       text=True, env=ENV)
    assert p.returncode == 0, (args, p.stdout, p.stderr)
    return p.stdout.strip()


def make_repo(tmp_path, record_opener, cite):
    """src/a.rs is LONG at commit C1 (the rev), SHORT in the index afterwards."""
    repo = tmp_path / "repo"
    repo.mkdir()
    git(repo, "init", "-q")
    (repo / "src").mkdir()
    (repo / "src/a.rs").write_text(LONG)
    git(repo, "add", "src/a.rs")
    git(repo, "commit", "-q", "-m", "c1")
    rev = git(repo, "rev-parse", "HEAD")
    (repo / "src/a.rs").write_text(SHORT)
    (repo / "docs").mkdir()
    opener = record_opener.replace("<REV>", rev)
    (repo / "docs/d.md").write_text(f"# d\n\n{opener}\n{cite}\n<!-- /record -->\n")
    git(repo, "add", "-A")
    return repo, rev


def run(repo):
    return subprocess.run([sys.executable, str(SCRIPT), "--repo", str(repo)],
                          capture_output=True, text=True, env=ENV, timeout=60)


OK_OPEN = "<!-- record: 2026-10-01 rev: <REV> -->"


def test_a_citation_valid_at_rev_but_out_of_range_at_index_passes(tmp_path):
    repo, _ = make_repo(tmp_path, OK_OPEN, "`src/a.rs:8` 「const NEEDLE: u8 = 1;」")
    r = run(repo)
    assert r.returncode == 0, (r.returncode, r.stdout, r.stderr)


def test_b1_quote_not_at_cited_line_at_rev_blocks(tmp_path):
    # quote 「// three」 is at line 2 in the INDEX (valid there) but not at rev.
    repo, _ = make_repo(tmp_path, OK_OPEN, "`src/a.rs:2` 「// three」")
    r = run(repo)
    assert r.returncode == 1, (r.returncode, r.stdout, r.stderr)


def test_b2_path_exists_in_index_but_not_at_rev_blocks(tmp_path):
    repo, _ = make_repo(tmp_path, OK_OPEN, "`src/new.rs:1`")
    (repo / "src/new.rs").write_text("// added after rev\n")
    git(repo, "add", "src/new.rs")
    r = run(repo)
    assert r.returncode == 1, (r.returncode, r.stdout, r.stderr)


def test_b3_path_missing_everywhere_blocks(tmp_path):
    repo, _ = make_repo(tmp_path, OK_OPEN, "`src/nope.rs:1`")
    r = run(repo)
    assert r.returncode == 1, (r.returncode, r.stdout, r.stderr)


def test_c_record_opener_without_rev_blocks_and_says_so(tmp_path):
    repo, _ = make_repo(tmp_path, "<!-- record: 2026-10-01 -->", "`src/a.rs:1`")
    r = run(repo)
    out = r.stdout + r.stderr
    assert r.returncode == 1, (r.returncode, out)
    assert "rev" in out.lower() and "record" in out.lower(), out


def test_d_record_opener_with_unresolvable_rev_blocks_and_says_so(tmp_path):
    repo, _ = make_repo(tmp_path, "<!-- record: 2026-10-01 rev: deadbeef0000 -->",
                        "`src/a.rs:1`")
    r = run(repo)
    out = r.stdout + r.stderr
    assert r.returncode == 1, (r.returncode, out)
    assert "deadbeef0000" in out, out
