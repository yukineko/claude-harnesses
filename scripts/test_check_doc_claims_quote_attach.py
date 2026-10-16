#!/usr/bin/env python3
"""Regression tests for quote ATTACHMENT in check-doc-claims.py (parser false
positives ruled so by the user).

A claim's quote is the first QUOTE_RE match in the rest of the line. Two
misreads were observed in a real doc:

  (M1) BACKTICK PARITY. The backtick arm can start at the CLOSING backtick of a
       no-whitespace code span and take the GAP between two code spans as a
       "quote". A backticked quote must be an actual code span (backticks
       paired left to right from the start of the line).
  (M2) MID-CELL SPAN IN A LATER TABLE CELL. In a table row, a code span sitting
       in the middle of the NEXT cell's prose was taken as the claim's quote.
       Once an unescaped table-cell `|` (outside code spans) lies between the
       claim and the candidate, the candidate is a quote ONLY if it is the
       first thing in its cell; otherwise the claim has no quote.

Both must hold for the backticked claim form (anywhere) and the unwrapped form
(record blocks only). Controls 5-9 pin that the fix does not loosen the gate.

Written by an independent tester; fixture style mirrors
scripts/test_check_doc_claims_record_unwrapped.py.

Run: python3 -m pytest -q scripts/test_check_doc_claims_quote_attach.py
"""
import json
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

FILLER = "".join(f"// filler {i}\n" for i in range(2, 31))  # lines 2..30
# Line 1 is valid for every claim; `let _ = git;` appears NOWHERE; none of the
# inter-span gaps used below ("** (note ", " then text ", " と ", "prose ")
# appear either.
BASE = "fn ident() -> u8 { 1 }\n" + FILLER
# The legitimate quote IS on line 1.
QUOTE_ON_LINE_1 = "fn decide() { let _ = git; }\n" + FILLER
# `let _ = git;` exists, but only at line 30 -- far from a :1 citation.
QUOTE_FAR_AWAY = "fn ident() -> u8 { 1 }\n" + "".join(
    f"// filler {i}\n" for i in range(2, 30)
) + "let _ = git;\n"

TABLE_HEAD = "| site | note |\n|---|---|\n"


def git(repo, *args):
    p = subprocess.run(["git", "-C", str(repo), *args], capture_output=True,
                       text=True, env=ENV)
    assert p.returncode == 0, (args, p.stdout, p.stderr)
    return p.stdout.strip()


def make_repo(tmp_path, body, content=BASE):
    """src/r.rs has `content` both at commit C1 (the record rev) and in the
    index, so record-judged and index-judged claims see the same file.
    `<REV>` in `body` is replaced with C1."""
    repo = tmp_path / "repo"
    repo.mkdir()
    git(repo, "init", "-q")
    (repo / "src").mkdir()
    (repo / "src/r.rs").write_text(content)
    git(repo, "add", "src/r.rs")
    git(repo, "commit", "-q", "-m", "c1")
    rev = git(repo, "rev-parse", "HEAD")
    (repo / "docs").mkdir()
    (repo / "docs/d.md").write_text("# d\n\n" + body.replace("<REV>", rev) + "\n")
    git(repo, "add", "-A")
    return repo


def record(inner):
    return f"<!-- record: 2026-10-01 rev: <REV> -->\n{inner}\n<!-- /record -->\n"


def run(repo):
    return subprocess.run(
        [sys.executable, str(SCRIPT), "--repo", str(repo), "--json"],
        capture_output=True, text=True, env=ENV, timeout=60,
    )


def kinds(r):
    if not r.stdout.strip():
        return []
    return [f["kind"] for f in json.loads(r.stdout)["findings"] if not f["exempt"]]


# --- misreads (must NOT block) ---------------------------------------------

def test_1_m1_unwrapped_record_gap_between_spans_is_not_a_quote(tmp_path):
    repo = make_repo(tmp_path, record("**P6 src/r.rs:1 — `ident`** (note `Other` here)"))
    r = run(repo)
    assert r.returncode == 0, (r.returncode, r.stdout, r.stderr)
    assert "quote-not-found" not in kinds(r), r.stdout


def test_2_m1_backticked_gap_between_spans_is_not_a_quote(tmp_path):
    repo = make_repo(tmp_path, "`src/r.rs:1` — `ident` then text `Other` end")
    r = run(repo)
    assert r.returncode == 0, (r.returncode, r.stdout, r.stderr)
    assert "quote-not-found" not in kinds(r), r.stdout


def test_3_m2_unwrapped_record_midcell_span_is_not_a_quote(tmp_path):
    repo = make_repo(
        tmp_path, record(TABLE_HEAD + "| src/r.rs:1 | prose `let _ = git;` more |")
    )
    r = run(repo)
    assert r.returncode == 0, (r.returncode, r.stdout, r.stderr)
    assert "quote-not-found" not in kinds(r), r.stdout


def test_4_m2_backticked_midcell_span_is_not_a_quote(tmp_path):
    repo = make_repo(
        tmp_path, TABLE_HEAD + "| `src/r.rs:1` | prose `let _ = git;` more |"
    )
    r = run(repo)
    assert r.returncode == 0, (r.returncode, r.stdout, r.stderr)
    assert "quote-not-found" not in kinds(r), r.stdout


# --- controls (must keep checking) -----------------------------------------

def test_5_backticked_whole_next_cell_wrong_quote_blocks(tmp_path):
    repo = make_repo(tmp_path, TABLE_HEAD + "| `src/r.rs:1` | `let _ = git;` |")
    r = run(repo)
    assert r.returncode == 1, (r.returncode, r.stdout, r.stderr)
    assert kinds(r) == ["quote-not-found"], r.stdout


def test_6_backticked_whole_next_cell_right_quote_passes(tmp_path):
    repo = make_repo(
        tmp_path, TABLE_HEAD + "| `src/r.rs:1` | `let _ = git;` |", QUOTE_ON_LINE_1
    )
    r = run(repo)
    assert r.returncode == 0, (r.returncode, r.stdout, r.stderr)
    assert kinds(r) == [], r.stdout


def test_7_unwrapped_record_whole_next_cell_wrong_quote_blocks(tmp_path):
    repo = make_repo(tmp_path, record(TABLE_HEAD + "| src/r.rs:1 | `let _ = git;` |"))
    r = run(repo)
    assert r.returncode == 1, (r.returncode, r.stdout, r.stderr)
    assert kinds(r) == ["quote-not-found"], r.stdout


def test_8_prose_span_after_claim_still_attaches(tmp_path):
    repo = make_repo(tmp_path, "`src/r.rs:1` の `let _ = git;`", QUOTE_FAR_AWAY)
    r = run(repo)
    assert r.returncode == 1, (r.returncode, r.stdout, r.stderr)
    ks = kinds(r)
    assert len(ks) == 1 and ks[0] in ("quote-not-found", "line-drifted"), r.stdout


def test_9_real_span_after_nonwhitespace_span_is_still_taken(tmp_path):
    repo = make_repo(tmp_path, "`src/r.rs:1` は `ident` と `wrong text here`")
    r = run(repo)
    assert r.returncode == 1, (r.returncode, r.stdout, r.stderr)
    assert kinds(r) == ["quote-not-found"], r.stdout
