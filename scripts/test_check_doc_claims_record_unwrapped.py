#!/usr/bin/env python3
"""Tests for UNWRAPPED `path:line` citations inside record blocks
(user ruling 2026-10-16): inside a record (`<!-- record: DATE rev: SHA -->` ..
`<!-- /record -->`, or the inline `[record: DATE rev: SHA]` form) the
doc-claims gate must ALSO check citations written WITHOUT backticks -- e.g. in
a markdown table cell or in prose -- against that record's rev, exactly as it
already checks backticked ones. Outside record blocks the scope stays
backticked-only.

Written by an independent tester BEFORE the implementation; the fixture style
mirrors scripts/test_check_doc_claims_record_rev.py.

Mirroring decisions (what the backticked record path does today, read from
scripts/check-doc-claims.py check_record_claim / CLAIM_RE / QUOTE_RE):
  * quotes: a 「…」 quote following a backticked record claim on the same line
    must be present AT the cited line at the rev (no drift tolerance), else
    `quote-not-found`. The unwrapped tests mirror that.
  * comma lists: CLAIM_RE requires the closing backtick right after the digits,
    so a backticked `x.rs:1848,1850` is NOT a claim at all -- the backticked
    path has no comma-list support. Per instruction, only the FIRST number of
    an unwrapped comma list is asserted on.
  * inline `[record: DATE rev: SHA]`: check-doc-claims.py has no support for
    the inline form at all (neither backticked nor unwrapped). The test here
    asserts only on a citation on the SAME line as the marker, the narrowest
    reading of the ruling.

Run: python3 -m pytest -q scripts/test_check_doc_claims_record_unwrapped.py
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

# 10 lines at the record rev; NEEDLE on line 8.
LONG = (
    "".join(f"// line {i}\n" for i in range(1, 8))
    + "const NEEDLE: u8 = 1;\n// line 9\n// line 10\n"
)
# 3 lines in the index (after the rev).
SHORT = "// only\n// three\n// lines\n"


def git(repo, *args):
    p = subprocess.run(["git", "-C", str(repo), *args], capture_output=True,
                       text=True, env=ENV)
    assert p.returncode == 0, (args, p.stdout, p.stderr)
    return p.stdout.strip()


def make_repo(tmp_path, body):
    """src/a.rs is LONG at commit C1 (the record rev), SHORT in the index.

    `body` is the document text after the title; `<REV>` is replaced with C1.
    """
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
    (repo / "docs/d.md").write_text("# d\n\n" + body.replace("<REV>", rev))
    git(repo, "add", "-A")
    return repo, rev


def record(inner):
    return f"<!-- record: 2026-10-01 rev: <REV> -->\n{inner}\n<!-- /record -->\n"


TABLE_HEAD = "| site | note |\n|---|---|\n"


def run(repo):
    return subprocess.run(
        [sys.executable, str(SCRIPT), "--repo", str(repo), "--json"],
        capture_output=True, text=True, env=ENV, timeout=60,
    )


def findings(r):
    import json
    return json.loads(r.stdout)["findings"] if r.stdout.strip() else []


def has(r, path, line, kind):
    return any(
        f["path"] == path and f["cited_line"] == line and f["kind"] == kind
        and not f["exempt"]
        for f in findings(r)
    )


# 1 -------------------------------------------------------------------------
def test_1_unwrapped_table_cell_past_eof_at_rev_blocks(tmp_path):
    # a.rs has 10 lines at rev; :20 is past EOF there.
    repo, _ = make_repo(tmp_path, record(TABLE_HEAD + "| src/a.rs:20 | gone |"))
    r = run(repo)
    assert r.returncode == 1, (r.returncode, r.stdout, r.stderr)
    assert has(r, "src/a.rs", 20, "line-out-of-range"), r.stdout


# 2 -------------------------------------------------------------------------
def test_2_unwrapped_in_range_at_rev_passes_though_out_of_range_at_index(tmp_path):
    # :8 is past EOF in the index (3 lines) but in range, quote included, at rev.
    repo, _ = make_repo(
        tmp_path,
        record(TABLE_HEAD + "| src/a.rs:8 「const NEEDLE: u8 = 1;」 | ok |"),
    )
    r = run(repo)
    assert r.returncode == 0, (r.returncode, r.stdout, r.stderr)


# 3 -------------------------------------------------------------------------
def test_3_unwrapped_quote_not_at_cited_line_at_rev_blocks(tmp_path):
    # 「// three」 is line 2 in the INDEX but line 2 at rev is "// line 2".
    repo, _ = make_repo(tmp_path, record("prose cites src/a.rs:2 「// three」 here."))
    r = run(repo)
    assert r.returncode == 1, (r.returncode, r.stdout, r.stderr)
    assert has(r, "src/a.rs", 2, "quote-not-found"), r.stdout


# 4 -------------------------------------------------------------------------
def test_4_unwrapped_comma_list_first_number_checked(tmp_path):
    # Backticked path has no comma-list support; only the first number (99,
    # past EOF at rev) is asserted on. 8 is valid at rev.
    repo, _ = make_repo(tmp_path, record(TABLE_HEAD + "| src/a.rs:99,8 | list |"))
    r = run(repo)
    assert r.returncode == 1, (r.returncode, r.stdout, r.stderr)
    assert has(r, "src/a.rs", 99, "line-out-of-range"), r.stdout


# 5 -------------------------------------------------------------------------
def test_5_unwrapped_outside_record_still_ignored(tmp_path):
    # Same doc has a real record (so record handling is active), but the
    # broken unwrapped citation sits OUTSIDE it, in prose and in a table.
    body = (
        record("`src/a.rs:8` 「const NEEDLE: u8 = 1;」")
        + "\nprose outside the record cites src/a.rs:20 and src/nope.rs:1.\n\n"
        + TABLE_HEAD + "| src/a.rs:99 | outside |\n"
    )
    repo, _ = make_repo(tmp_path, body)
    r = run(repo)
    assert r.returncode == 0, (r.returncode, r.stdout, r.stderr)


# 6 -------------------------------------------------------------------------
def test_6_unwrapped_path_missing_at_rev_blocks(tmp_path):
    repo, _ = make_repo(tmp_path, record(TABLE_HEAD + "| src/new.rs:1 | added later |"))
    # Exists in the index but NOT at the record rev: still fail-closed.
    (repo / "src/new.rs").write_text("// added after rev\n")
    git(repo, "add", "src/new.rs")
    r = run(repo)
    assert r.returncode == 1, (r.returncode, r.stdout, r.stderr)
    assert has(r, "src/new.rs", 1, "path-not-found"), r.stdout


# 7 (inline form) -----------------------------------------------------------
def test_7_inline_record_unwrapped_judged_at_rev_blocks(tmp_path):
    # 「// three」 IS at line 2 in the index, so only judging at the rev fails it.
    repo, _ = make_repo(
        tmp_path,
        "[record: 2026-10-01 rev: <REV>] the site was src/a.rs:2 「// three」.\n",
    )
    r = run(repo)
    assert r.returncode == 1, (r.returncode, r.stdout, r.stderr)
    assert has(r, "src/a.rs", 2, "quote-not-found"), r.stdout


# control -------------------------------------------------------------------
def test_0_control_backticked_twin_of_test_1_blocks(tmp_path):
    # Proves the fixture and the `has` helper can observe a finding at all:
    # the backticked twin of test 1 is already checked today.
    repo, _ = make_repo(tmp_path, record(TABLE_HEAD + "| `src/a.rs:20` | gone |"))
    r = run(repo)
    assert r.returncode == 1, (r.returncode, r.stdout, r.stderr)
    assert has(r, "src/a.rs", 20, "line-out-of-range"), r.stdout
