#!/usr/bin/env python3
"""Verify that `path:line` claims in docs/**/*.md still describe reality.

Why this gate exists
--------------------
CLAUDE.md was found carrying five false statements, all written hours earlier.
Three of them were mechanically detectable: a norm described as "removed" that
was still live at the very `path:line` the document cited, a type described as
existing that `grep` finds zero of, and a defect written about in the past tense
that is still present. A record that has rotted is not a harmless record — the
next implementer reasons from it, so a stale document does the same damage as a
docstring that lies about its own code, which this repo treats as the worst act.

Scope note: CLAUDE.md is an instruction/config file, not a documentation page,
and folding it into a generic doc-citation gate blurred two different concerns.
CLAUDE.md's claims are now verified by the DEDICATED gate
`scripts/check-claudemd-claims.py`, which reuses the exact same engine defined
in this module. THIS gate's scope is `docs/**/*.md` only.

Prose cannot be checked in general. A *citation* can. So this gate checks the
part of the prose that carries a machine-verifiable commitment: the cited path,
the cited line, and any verbatim quote attached to them.

WHICH TREE IS JUDGED — the git INDEX, not the working tree
----------------------------------------------------------
This is a PRE-COMMIT gate, so the artifact it has an opinion about is the tree
the commit will RECORD: the git index. By default (`--source index`) both the
documents AND the cited files are read from the index at stage 0, via
`git ls-files -s -z` for the scope and `git cat-file blob :0:<path>` for the
content. `--source worktree` restores the old behaviour for ad-hoc use.

It used to read both sides from the working tree, and that was wrong in a way
ordinary parallel work triggers. Measured on this repo at commit `9a4e400c`:
prepending 13 blank lines to `crates/condukt/skills/condukt/SKILL.md` WITHOUT
staging them made `--doc docs/plugin-dependency-graph.md` block with five
`line-drifted` findings, e.g. "quote is at line(s) 143 but the claim cites 130".
The index was untouched and every one of those citations was still correct in
it, so the gate blocked a commit that was correct, on the strength of a peer
session's unstaged edit. `LINE_DRIFT_TOLERANCE` only hides drift smaller than
11 lines, and whether you land inside it depends on how much someone else
happened to edit. The two shortest ways out of that block were the gate-bypass
flag, or rewriting the document's line numbers to match the peer's uncommitted
tree — which writes a citation that is FALSE for the commit being made, i.e.
exactly the rot this gate exists to prevent. A gate whose own pressure pushes
toward defeating it is a broken gate.

ONE SOURCE, CONSISTENTLY. Whichever mode is selected, the document text and the
cited file's text come from the SAME artifact. Reading the documents from one
and the cited files from the other would be a fresh fail-open of its own: the
gate would be checking a tree that never existed and will never be committed.

This gate reads the WHOLE index every run rather than only the staged diff, and
that is what makes index mode complete rather than merely convenient: every
commit is judged against the entire tree it records, so a commit that moves
code without touching the document that cites it is still caught — the document
comes from the index too, unchanged, and the drift shows up.

Contrast with `check-prompt-injection.py` and `check-clippy-lints.py`, which
deliberately scan the WORKING TREE. That is not an inconsistency: those gates
hunt for bad content being INTRODUCED, so the widest superset is the strict
answer and reading only the index would let an author stage the safe half. This
gate answers a different question — "is this citation TRUE of the tree being
recorded" — and for that question the index is the only artifact the claim is
about. The invariant both obey is the same one: the source you read must be the
artifact your verdict is about.

Claim syntax
------------
A claim is a backtick-quoted `<path>:<line>` token. Any verbatim quote that
follows it ON THE SAME LINE, delimited by `「 」`, `" "` or backticks, is taken
as part of the claim. Write claims that way and they stay checkable; write them
as loose prose and this gate cannot help you.

A backticked quote must be a real code span (backticks paired left to right
from the start of the line), never the text between two spans. When a table
cell `|` lies between the claim and the quote, the quote must be the first
thing in its cell, as in  | `x.rs:3` | `let a = b;` |  ; a span mid-prose in a
later cell is not attached, and the claim is then checked for path and line
only. This applies to backticked and unwrapped claims alike.

Exception, record scope only (user ruling 2026-10-16): on a line judged at a
record's rev (see "Record blocks" below), an UNWRAPPED `<path>:<line>` token --
in a table cell, in prose, or inside a larger backticked span -- is a claim
too, with the same same-line quote rule. The path must carry a file-ish
extension; for a comma list (`x.rs:12,14`) only the first number is taken.
Outside records the claim syntax stays backticked-only.

Findings
--------
  path-escapes-repo  the cited path resolves outside the repository root
  path-not-found     the cited path does not exist
  line-out-of-range  the file is shorter than the cited line
  quote-not-found    the quote occurs nowhere in the cited file
  line-drifted       the quote is real but lives far from the cited line

Fail-closed contract
--------------------
  exit 0  every claim checks out
  exit 1  at least one unexempted finding                    -> block
  exit 2  the verdict could not be determined                -> block

No new exit code was added for index mode; every new failure mode below lands
in one of these three.

Note the deliberate asymmetry: a cited file that is MISSING is an answer about
the claim (`path-not-found`, exit 1); a cited file that EXISTS BUT CANNOT BE
READ is not an answer at all (exit 2). Collapsing the second into the first
would let an unreadable tree read as a documented one.

A cited path that is UNTRACKED — present in the working tree, absent from the
index — is `path-not-found` (exit 1), NOT undetermined. It sits on the MISSING
side of that asymmetry on purpose. Index mode judges the tree the commit will
record, `git ls-files` answers definitively whether a path is in it, and the
answer here is a flat no: whoever checks out that commit gets a document citing
a file the commit does not contain. That is a determinate fact about the claim,
not a failure to observe, so calling it undetermined would be a false statement
about the gate's own knowledge — and it would print "could not determine",
which points the author away from the two real fixes (stage the file in this
commit, or drop the citation). The consequence is deliberate: a document that
cites a brand-new file must be committed TOGETHER with that file.

The other candidate — "absent from the index is undetermined" — was BUILT AND
MEASURED rather than argued away, and it is worse in a way that is not obvious
from the principle alone. `path-not-found` is a finding, so the per-line
`doc-claim-exempt` marker applies to it; Undetermined is exit 2 and no
exemption reaches it. Run against this repo at `9a4e400c`, the undetermined
variant exited 2 on `crates/autoflow/src/compass.rs` — a deliberately deleted
file whose five citations in `docs/autoflow-verdict-audit.md` are already
exempted on purpose — and would therefore have blocked every commit in the
repository with no sanctioned way to clear it.

These, by contrast, genuinely cannot be observed, and are exit 2:

  * `--repo` is not a git repository, or is not its TOP LEVEL. `:0:<path>` is
    resolved against the top level, so judging from a subdirectory would
    silently misresolve every path in every claim.
  * `git` cannot be run at all, or any git call this gate depends on exits
    non-zero, or emits an `ls-files -s -z` record this module cannot parse.
  * a document or cited path is in the index but its blob is not valid UTF-8.
  * a cited path is in the index at a mode that is not a regular file blob
    (`120000` symlink, `160000` submodule): `cat-file` would hand back a link
    target or a commit id, and matching a quote against that is meaningless.
  * a cited path is UNMERGED (a conflict left stages 1/2/3 and no stage 0), so
    there is no single content the commit would record.

The default scope is `docs/**/*.md`, walked RECURSIVELY, and a scope that comes
out EMPTY is exit 2 rather than exit 0. Both halves guard the same failure: a
gate whose scope silently shrinks to nothing goes on reporting clean, and does
so most convincingly at the moment it has stopped checking anything.
CLAUDE.md is deliberately NOT in this default scope — see the dedicated
check-claudemd-claims.py gate above.

The scope follows the source, for the same one-source reason: in index mode it
is the `.md` files under `docs/` that are IN THE INDEX. An unstaged document is
therefore not scanned — it is not part of the commit — and a document that is
staged but deleted from the working tree still is.

Record blocks
-------------
Lines between `<!-- record: YYYY-MM-DD rev: <rev> -->` and `<!-- /record -->`
are a record measured at commit <rev> (user ruling 2026-10-15). Their citations
are checked against THAT commit, not the index: the path must exist at <rev>
(`git ls-tree`), the line must be in range in `git cat-file blob <rev>:<path>`,
and a quote must be present AT the cited line (no drift tolerance -- the record
says where it was). An opener with no rev, or whose rev does not resolve to a
commit, is NOT a record: it is reported as a blocking finding
(`record-without-rev` / `record-rev-unresolved`) and the citations inside are
judged against the index like any other. A git failure while reading <rev> is
undetermined (exit 2). An unterminated block or an invalid date exempts
nothing.

The inline form `[record: YYYY-MM-DD rev: <rev>]` may appear anywhere on a
line; the citations that FOLLOW it on that SAME line (not the rest of the
paragraph) are judged at <rev> exactly as above. A rev that does not resolve
is a blocking `record-rev-unresolved` finding and those citations are judged
against the index. An inline marker without a rev, or with an invalid date,
re-targets nothing (no finding): its citations are judged against the index,
backticked-only, like any other line.

Inside a record -- block lines and the post-marker part of an inline record
line -- unwrapped `<path>:<line>` tokens are claims as well (see "Claim
syntax"), with the same fail-closed findings: path absent at <rev> is
`path-not-found`, a line past its end `line-out-of-range`, a quote not at the
cited line `quote-not-found`.

Exemption
---------
Some claims describe a historical state on purpose. A line

    <!-- doc-claim-exempt: <reason> -->

immediately before the claim's line exempts the claims on that line. The reason
is mandatory and the scope is one line: a reasonless or file-wide exemption
would hand the next author a one-line way to switch the gate off, which is the
fail-open this gate exists to prevent. Exempted findings stay visible in the
report -- an exemption that also hides the claim would be unauditable.

Known non-goal (v1)
-------------------
Claims of the form "`grep -rn X path/` = N 件" are NOT verified. Running shell
fragments lifted out of a document is a different risk surface and needs its own
whitelist; until then those claims remain unchecked, and this paragraph exists so
that gap reads as a known limit rather than as coverage.
"""

from __future__ import annotations

import argparse
import glob
import json
import os
import posixpath
import re
import subprocess
import sys

# `path:line` inside backticks. The path must look like a path (no spaces, no
# backticks) and carry a file-ish extension, so ordinary prose such as
# `foo:12` in a log excerpt does not become a claim.
CLAIM_RE = re.compile(r"`(?P<path>[^`\s]+\.[A-Za-z0-9_]+):(?P<line>\d+)`")

# The three quote delimiters, tried together so the EARLIEST one after the
# claim wins regardless of kind.
#
# The backtick arm requires the span to CONTAIN WHITESPACE. In markdown prose a
# backticked span is far more often an identifier or another path than a
# verbatim quotation -- measured on this repo, taking the first backticked span
# produced three false positives out of six findings (`checks_verdict`,
# `run_ignored_test`, and a following `path:line` reference all got read as
# quotations). A gate that cries wolf gets switched off, so the ambiguous form
# is excluded and an author who wants a bare identifier checked writes it in
# 「」 or "" to say so explicitly.
#
# The 「」 and "" arms are this regex; the backtick arm is resolved against
# the line's REAL code spans by find_quote(), because a regex searched from the
# claim's end can start at a span's CLOSING backtick and take the text BETWEEN
# two spans as a "quote" (measured: `', '`, `' | '`, and a whole sentence of
# prose were attached as quotes this way).
DELIM_QUOTE_RE = re.compile(r"「(?P<jp>[^」]+)」|\"(?P<dq>[^\"]+)\"")


def code_spans(line: str) -> list:
    """The line's code spans as (open_idx, close_idx), backticks paired left to
    right from the START of the line. A backtick that closes one span can never
    open another, so the text BETWEEN two spans is never mistaken for one."""
    ticks = [i for i, ch in enumerate(line) if ch == "`"]
    return list(zip(ticks[0::2], ticks[1::2]))


def find_quote(line: str, end: int):
    """The quote attached to a claim ending at `end`, or None.

    The earliest candidate after the claim wins: a 「」 or "" quote, or a REAL
    code span (see code_spans) whose content contains whitespace. Then, if an
    unescaped table-cell `|` outside any code span lies between the claim and
    that candidate, the candidate counts only when it is the first thing in
    its cell (a 「」/"" quote inside a code span is anchored at that span's
    opening backtick). A span sitting mid-prose in a later cell is not a quote
    of this claim, and the claim then has no quote at all -- the path and line
    are still checked."""
    spans = code_spans(line)
    best = None
    dm = DELIM_QUOTE_RE.search(line, end)
    if dm:
        best = (dm.start(), dm.group("jp") or dm.group("dq"))
    for o, c in spans:
        if o < end:
            continue
        body = line[o + 1 : c]
        if any(ch.isspace() for ch in body):
            if best is None or o < best[0]:
                best = (o, body)
            break
    if best is None:
        return None
    qstart, quote = best
    for o, c in spans:
        if o < qstart <= c:
            qstart = o
            break
    last_pipe = None
    for i in range(end, qstart):
        if line[i] != "|" or (i > 0 and line[i - 1] == "\\"):
            continue
        if any(o <= i <= c for o, c in spans):
            continue
        last_pipe = i
    if last_pipe is not None and line[last_pipe + 1 : qstart].strip():
        return None
    return quote


EXEMPT_RE = re.compile(r"<!--\s*doc-claim-exempt\s*:\s*(?P<reason>\S.*?)\s*-->")

LINE_DRIFT_TOLERANCE = 10

# A record block (user ruling 2026-10-15): a record cites the tree AS IT WAS at
# the commit it names, so its citations are judged against that commit. Only a
# block that is CLOSED and whose opener carries a real calendar date is a
# candidate: an unterminated or undated opener would otherwise re-target
# everything after it. A candidate without a `rev:` is not a record (see
# record_opener_findings).
RECORD_OPEN_RE = re.compile(
    r"^\s*<!--\s*record:\s*(\d{4})-(\d{2})-(\d{2})(?:\s+rev:\s*(\S+?))?\s*-->\s*$"
)
RECORD_CLOSE_RE = re.compile(r"^\s*<!--\s*/record\s*-->\s*$")

# The inline record form, `[record: YYYY-MM-DD rev: <rev>]`, anywhere on a
# line. It re-targets only the citations that FOLLOW it on that same line. The
# rev is optional here (check-prose-tests.py accepts a revless inline record);
# a revless inline marker re-targets nothing, so its citations stay judged
# against the source like any other.
INLINE_RECORD_RE = re.compile(
    r"\[record:\s*(\d{4})-(\d{2})-(\d{2})(?:\s+rev:\s*([^\]\s]+))?\s*\]"
)

# An UNWRAPPED `path:line` (no backticks required). Used ONLY on lines judged
# at a record's rev (user ruling 2026-10-16); outside records the claim syntax
# stays backticked-only. The lookbehind makes the match start at the beginning
# of the path token rather than in its middle. For a comma list such as
# `x.rs:12,14` only the first number is taken.
UNWRAPPED_CLAIM_RE = re.compile(
    r"(?<![A-Za-z0-9_./\-])(?P<path>[A-Za-z0-9_./\-]+\.[A-Za-z0-9_]+):(?P<line>\d+)"
)

KIND_RECORD_WITHOUT_REV = "record-without-rev"
KIND_RECORD_REV_UNRESOLVED = "record-rev-unresolved"


def record_blocks(lines: list) -> list:
    """Closed, validly dated record blocks: [(open_idx, close_idx, rev_or_None)]."""
    import datetime

    blocks = []
    opened = None
    for idx, line in enumerate(lines):
        m = RECORD_OPEN_RE.match(line)
        if m:
            try:
                datetime.date(int(m.group(1)), int(m.group(2)), int(m.group(3)))
                opened = (idx, m.group(4))
            except ValueError:
                opened = None
            continue
        if RECORD_CLOSE_RE.match(line):
            if opened is not None:
                blocks.append((opened[0], idx, opened[1]))
            opened = None
    return blocks


class RevReader:
    """Reads files AT a commit, for record-block citations. Every git failure
    that is not a definite "absent" answer raises Undetermined."""

    def __init__(self, repo: str):
        self.repo = repo
        self._resolved: dict = {}
        self._lines: dict = {}

    def _git(self, *args: str):
        try:
            return subprocess.run(["git", "-C", self.repo, *args], capture_output=True)
        except OSError as exc:
            raise Undetermined(f"git could not be run: {exc}") from exc

    def resolve(self, rev: str):
        """The commit sha `rev` names, or None when it names no commit."""
        if rev not in self._resolved:
            proc = self._git("rev-parse", "--verify", "--quiet", f"{rev}^{{commit}}")
            out = proc.stdout.decode("utf-8", "replace").strip()
            if proc.returncode == 0 and out:
                self._resolved[rev] = out
            elif proc.returncode == 1 and not out:
                self._resolved[rev] = None
            else:
                raise Undetermined(
                    f"git rev-parse {rev!r} exited {proc.returncode}: "
                    f"{proc.stderr.decode('utf-8', 'replace').strip()}"
                )
        return self._resolved[rev]

    def lines(self, sha: str, rel: str):
        """The file's lines at `sha`, or None when the path is absent there."""
        key = (sha, rel)
        if key not in self._lines:
            ls = self._git("ls-tree", "-z", sha, "--", rel)
            if ls.returncode != 0:
                raise Undetermined(
                    f"git ls-tree {sha} -- {rel} exited {ls.returncode}: "
                    f"{ls.stderr.decode('utf-8', 'replace').strip()}"
                )
            entry = ls.stdout.split(b"\0")[0].decode("utf-8", "surrogateescape")
            if not entry:
                self._lines[key] = None
            elif not entry.split(" ", 2)[1:2] == ["blob"]:
                self._lines[key] = None
            else:
                blob = self._git("cat-file", "blob", f"{sha}:{rel}")
                if blob.returncode != 0:
                    raise Undetermined(
                        f"git cat-file blob {sha}:{rel} exited {blob.returncode}: "
                        f"{blob.stderr.decode('utf-8', 'replace').strip()}"
                    )
                self._lines[key] = source_lines(
                    blob.stdout.decode("utf-8", "surrogateescape")
                )
        return self._lines[key]


def record_opener_findings(doc_rel: str, lines: list, reader: RevReader):
    """Blocking findings for record markers that are not records, and the map
    0-based line index -> [(start_col, rev, sha), ...] for lines judged at a
    record's rev (see record_at). start_col is 0 inside a block record and the
    end of the marker for an inline `[record: DATE rev: SHA]`: only citations
    at or after it belong to that record."""
    findings = []
    inside: dict = {}
    for open_idx, close_idx, rev in record_blocks(lines):
        stub = {
            "doc": doc_rel,
            "doc_line": open_idx + 1,
            "path": "(record block)",
            "cited_line": 0,
            "exempt_reason": None,
        }
        if rev is None:
            findings.append(
                finding(
                    stub,
                    KIND_RECORD_WITHOUT_REV,
                    "a record block must name the commit it was measured at "
                    "(`<!-- record: YYYY-MM-DD rev: <sha> -->`); without a rev "
                    "it is not a record",
                )
            )
            continue
        sha = reader.resolve(rev)
        if sha is None:
            findings.append(
                finding(
                    stub,
                    KIND_RECORD_REV_UNRESOLVED,
                    f"record rev {rev!r} does not resolve to a commit; "
                    "it is not a record",
                )
            )
            continue
        for idx in range(open_idx + 1, close_idx):
            inside[idx] = [(0, rev, sha)]
    import datetime

    for idx, line in enumerate(lines):
        for m in INLINE_RECORD_RE.finditer(line):
            try:
                datetime.date(int(m.group(1)), int(m.group(2)), int(m.group(3)))
            except ValueError:
                continue
            rev = m.group(4)
            if rev is None:
                continue
            sha = reader.resolve(rev)
            if sha is None:
                findings.append(
                    finding(
                        {
                            "doc": doc_rel,
                            "doc_line": idx + 1,
                            "path": "(inline record)",
                            "cited_line": 0,
                            "exempt_reason": None,
                        },
                        KIND_RECORD_REV_UNRESOLVED,
                        f"inline record rev {rev!r} does not resolve to a "
                        "commit; it is not a record",
                    )
                )
                continue
            inside.setdefault(idx, []).append((m.end(), rev, sha))
    return findings, inside


def record_at(segments, col: int):
    """The (rev, sha) a citation starting at `col` is judged at, or None.

    `segments` is one line's [(start_col, rev, sha), ...] in ascending
    start_col; the last segment that starts at or before `col` wins, so a later
    inline marker on a line re-targets only what follows it."""
    hit = None
    for start, rev, sha in segments or ():
        if start <= col:
            hit = (rev, sha)
    return hit

KIND_PATH_ESCAPES_REPO = "path-escapes-repo"
KIND_PATH_NOT_FOUND = "path-not-found"
KIND_LINE_OUT_OF_RANGE = "line-out-of-range"
KIND_QUOTE_NOT_FOUND = "quote-not-found"
KIND_LINE_DRIFTED = "line-drifted"

SOURCE_INDEX = "index"
SOURCE_WORKTREE = "worktree"
SOURCES = (SOURCE_INDEX, SOURCE_WORKTREE)

# What a path in the tree can be, as far as this gate is concerned.
PRESENT_FILE = "file"
ABSENT = "absent"

# `git ls-files -s -z` record: "<mode> <object> <stage>\t<path>". `.*` with
# re.S because a path may contain a newline -- that is why -z is used at all.
LS_FILES_RE = re.compile(
    r"^(?P<mode>\d{6}) (?P<oid>[0-9a-f]+) (?P<stage>\d)\t(?P<path>.*)$", re.S
)

# The index modes that denote an ordinary file whose blob is its content.
# 120000 (symlink) and 160000 (gitlink/submodule) are deliberately excluded --
# see the module docstring.
BLOB_FILE_MODES = frozenset({"100644", "100755"})


class Undetermined(Exception):
    """The verdict could not be established. Always resolves to exit 2."""


def norm_ws(s: str) -> str:
    return " ".join(s.split())


def rel_key(rel: str) -> str:
    """The canonical spelling of a repo-relative path, for index lookups.

    Claims are written by hand, so `docs/./x.md` and `docs/x.md` must reach the
    same index entry. Purely lexical -- there is no filesystem to consult when
    the artefact under judgment is the index.
    """
    return posixpath.normpath(rel)


def lexically_escapes_repo(rel: str) -> bool:
    """True when the STRING alone proves the path cannot be inside the repo.

    Absolute, empty, or normalising to `..`/`../...`. This is the whole test in
    index mode (index paths are repo-relative by construction, so anything that
    is not is out of scope), and the FIRST test in worktree mode, which then
    also resolves symlinks.
    """
    if not rel:
        return True
    if posixpath.isabs(rel):
        return True
    norm = posixpath.normpath(rel)
    return norm == ".." or norm.startswith("../")


def source_lines(src: str) -> list:
    """Split like git, an editor and a compiler do: on newlines, and only those.

    Neither `split("\\n")` nor `splitlines()` is correct here, and both fail in
    the permissive direction -- they overcount, so a citation past the real end
    of the file passes the range check.

      * `split("\\n")` leaves a trailing "" for the final newline every text
        file has. Measured: a 2-line file accepted `:3`, and the message for
        `:4` read "file has 3 lines".
      * `splitlines()` additionally breaks on FORM FEED, VT, NEL and U+2028/9,
        which no line-numbering tool in this toolchain treats as a line break.
        Measured: a 2-line file containing one `\\x0c` reported 3 lines and
        accepted `:3`. This was introduced while fixing the case above -- the
        repair of one overcount produced another.

    So: fold CRLF/CR, split on "\\n" alone, and drop the single empty element a
    trailing newline leaves behind.
    """
    body = src.replace("\r\n", "\n").replace("\r", "\n")
    lines = body.split("\n")
    if lines and lines[-1] == "":
        lines.pop()
    return lines


class WorktreeSource:
    """The files as they sit on disk. `--source worktree`.

    Kept for ad-hoc use ("does the document I am editing check out right now?")
    and as the control against which index mode is measured. It is NOT what the
    pre-commit hook uses -- see the module docstring.
    """

    name = SOURCE_WORKTREE
    absent_detail = "no such file"

    def __init__(self, repo: str):
        self.repo = repo

    def doc_rels(self) -> list:
        pattern = os.path.join(self.repo, "docs", "**", "*.md")
        return sorted(
            os.path.relpath(full, self.repo)
            for full in glob.glob(pattern, recursive=True)
        )

    def has_doc(self, rel: str) -> bool:
        return os.path.isfile(os.path.join(self.repo, rel))

    def escapes_repo(self, rel: str) -> bool:
        # A claim that resolves outside the repository is not checkable,
        # whichever way it happens to come out. Its verdict would depend on the
        # machine -- green on the runner, red on a laptop, or the reverse --
        # and a verdict that changes with the filesystem around the repo is not
        # a fact about the repo. It is also an unnecessary read primitive: doc
        # text arrives with the diff, so `../..`-style claims would let a
        # document decide what the gate opens and whether the line matched.
        if lexically_escapes_repo(rel):
            return True
        target = os.path.realpath(os.path.join(self.repo, rel))
        root = os.path.realpath(self.repo)
        return target != root and not target.startswith(root + os.sep)

    def classify(self, rel: str) -> str:
        return (
            PRESENT_FILE
            if os.path.isfile(os.path.join(self.repo, rel))
            else ABSENT
        )

    def prefetch(self, rels) -> None:
        """No-op. Reading a file off disk is already one syscall."""

    def read(self, rel: str, what: str) -> str:
        """Read a file, treating every failure as undetermined.

        Callers must have established that the file EXISTS before calling: a
        missing file is a finding about the claim, an unreadable one is a
        failure to observe, and the two must not share a branch.
        """
        path = os.path.join(self.repo, rel)
        try:
            with open(path, "r", encoding="utf-8") as fh:
                return fh.read()
        except (OSError, UnicodeDecodeError) as exc:
            raise Undetermined(f"{what} {path} could not be read: {exc}") from exc


class IndexSource:
    """The tree the commit would RECORD: git index stage 0. `--source index`.

    The whole index is listed ONCE in the constructor, so "is this path in the
    commit" is answered from a snapshot rather than re-shelled per claim, and
    every claim in one run is judged against one consistent state.
    """

    name = SOURCE_INDEX
    absent_detail = "not staged in the git index"

    def __init__(self, repo: str):
        self.repo = repo
        self._blobs: dict = {}

        top = self._git("rev-parse", "--show-toplevel").decode(
            "utf-8", "surrogateescape"
        ).strip()
        if os.path.realpath(top) != os.path.realpath(repo):
            # `:0:<path>` is resolved against the TOP LEVEL, and `git ls-files`
            # run from a subdirectory lists only that subdirectory. Judging
            # from anywhere but the top would therefore misresolve every path
            # AND silently shrink the scope, both without a word.
            raise Undetermined(
                f"{repo} is not the top level of its git repository "
                f"(top level is {top}); index paths would be resolved against "
                "the wrong root, so no claim in it can be judged"
            )

        # Stage 0 is the ordinary, merged index entry -- the content a commit
        # made right now would record. Stages 1/2/3 only exist during an
        # unresolved conflict and are tracked separately so that state reads as
        # undetermined instead of as "the path is absent".
        self.stage0: dict = {}
        self.unmerged: set = set()
        for record in self._git("ls-files", "-s", "-z").split(b"\0"):
            if not record:
                continue
            text = record.decode("utf-8", "surrogateescape")
            m = LS_FILES_RE.match(text)
            if not m:
                # An unparseable record means this function does not know what
                # git just said. Skipping it would drop a path out of scope
                # without a word, which is the failure mode this gate exists
                # to close.
                raise Undetermined(
                    f"unparseable `git ls-files -s -z` record: {text!r}"
                )
            path = rel_key(m.group("path"))
            if m.group("stage") == "0":
                self.stage0[path] = m.group("mode")
            else:
                self.unmerged.add(path)

    def _git(self, *args: str) -> bytes:
        """Run git and hand back raw stdout, or raise Undetermined.

        Bytes, not text: a blob that is not valid UTF-8 must reach a decode we
        control, so that failure lands on the undetermined branch instead of
        being papered over by an error handler chosen by subprocess.
        """
        cmd = ["git", "-C", self.repo, *args]
        try:
            proc = subprocess.run(cmd, capture_output=True)
        except OSError as exc:
            raise Undetermined(
                f"could not run `git {' '.join(args)}` in {self.repo}: {exc}"
            ) from exc
        if proc.returncode != 0:
            detail = proc.stderr.decode("utf-8", "replace").strip() or "(no stderr)"
            raise Undetermined(
                f"`git {' '.join(args)}` exited {proc.returncode} in "
                f"{self.repo}: {detail}"
            )
        return proc.stdout

    def _require_blob_mode(self, rel: str, mode: str) -> None:
        if mode not in BLOB_FILE_MODES:
            raise Undetermined(
                f"{rel} is in the index at mode {mode}, which is not a regular "
                "file (120000 is a symlink, 160000 a submodule); its blob is a "
                "link target or a commit id, not the text a claim is about"
            )

    def doc_rels(self) -> list:
        rels = sorted(
            p
            for p in self.stage0
            if p.startswith("docs/") and p.endswith(".md")
        )
        for rel in rels:
            self._require_blob_mode(rel, self.stage0[rel])
        return rels

    def has_doc(self, rel: str) -> bool:
        return rel_key(rel) in self.stage0

    def escapes_repo(self, rel: str) -> bool:
        # Lexical only, and that is complete here: every index path is
        # repo-relative and normalised by git, so a claim that does not
        # normalise to a repo-relative path cannot name an index entry at all.
        # There is deliberately no realpath() call -- resolving against the
        # working tree would reintroduce exactly the cross-source read this
        # mode exists to remove.
        return lexically_escapes_repo(rel)

    def classify(self, rel: str) -> str:
        key = rel_key(rel)
        mode = self.stage0.get(key)
        if mode is not None:
            self._require_blob_mode(rel, mode)
            return PRESENT_FILE
        if key in self.unmerged:
            raise Undetermined(
                f"{rel} is UNMERGED in the index (a conflict left stages 1/2/3 "
                "and no stage 0), so there is no single content the commit "
                "would record and no claim about it can be judged"
            )
        # Untracked, or staged for deletion. Determinately not in the tree the
        # commit records -- an ANSWER about the claim, not a failure to look.
        # See the module docstring for why this is exit 1 and not exit 2.
        return ABSENT

    def prefetch(self, rels) -> None:
        """Warm the blob cache for `rels` with ONE `git cat-file --batch` call.

        This is a cache and NOTHING ELSE. It never decides anything, never
        raises, and never records a failure: whatever it cannot fetch or cannot
        decode is simply left uncached, and `read()` — the authoritative path —
        re-fetches it and resolves it the fail-closed way. That separation is
        deliberate. An optimisation that can also produce a verdict is an
        optimisation that can produce a WRONG verdict, and this gate's whole
        subject is gates that answer from the wrong artifact.

        It exists because the one-shot path costs a process per file: measured
        on this repo (88 documents, 54 distinct cited paths), `--source index`
        took 1.89s with per-file `cat-file` and 0.17s with this batch, against
        0.11s for `--source worktree`. A ~1.8s tax on every commit is the kind
        of thing that gets a gate switched off.
        """
        keys = []
        for rel in rels:
            key = rel_key(rel)
            # `cat-file --batch` is a line protocol, so a path containing a
            # newline cannot be requested through it. Such a path is left to
            # read()'s one-shot form, which passes it in argv.
            if key in self._blobs or "\n" in key or key in keys:
                continue
            keys.append(key)
        if not keys:
            return

        payload = "".join(f":0:{k}\n" for k in keys).encode("utf-8", "surrogateescape")
        try:
            proc = subprocess.run(
                ["git", "-C", self.repo, "cat-file", "--batch"],
                input=payload,
                capture_output=True,
            )
        except OSError:
            return
        if proc.returncode != 0:
            return

        out, pos = proc.stdout, 0
        for key in keys:
            nl = out.find(b"\n", pos)
            if nl < 0:
                return
            header, pos = out[pos:nl], nl + 1
            parts = header.split(b" ")
            if len(parts) == 2 and parts[1] in (b"missing", b"ambiguous"):
                # No payload follows, so the offset stays valid; skip this one.
                continue
            if len(parts) != 3 or parts[1] != b"blob":
                # A shape this parser does not know. Every later entry's offset
                # is now unknown, so stop rather than cache misaligned bytes.
                return
            try:
                size = int(parts[2])
            except ValueError:
                return
            raw, pos = out[pos : pos + size], pos + size + 1
            if len(raw) != size:
                return
            try:
                self._blobs[key] = raw.decode("utf-8")
            except UnicodeDecodeError:
                # Not an answer. Leave it uncached so read() raises Undetermined.
                continue

    def read(self, rel: str, what: str) -> str:
        key = rel_key(rel)
        if key in self._blobs:
            return self._blobs[key]
        # The argument always begins with ':', so it can never be mistaken for
        # an option however the path is spelled, and the explicit `0` stage
        # keeps a path containing ':' from being read as a stage selector.
        raw = self._git("cat-file", "blob", f":0:{key}")
        try:
            text = raw.decode("utf-8")
        except UnicodeDecodeError as exc:
            raise Undetermined(
                f"{what} {rel} is in the index but its blob is not valid "
                f"UTF-8: {exc}"
            ) from exc
        self._blobs[key] = text
        return text


def make_source(repo: str, name: str):
    """Build the one source both halves of the scan will read from."""
    if name == SOURCE_WORKTREE:
        return WorktreeSource(repo)
    if name == SOURCE_INDEX:
        return IndexSource(repo)
    # Unreachable through argparse's `choices`, but an unknown source must not
    # quietly become a default one.
    raise Undetermined(f"unknown --source {name!r}; expected one of {SOURCES}")


def extract_claims(doc_rel: str, text: str, in_record: dict | None = None) -> list:
    """(doc, doc_line, path, cited_line, quote_or_None, exempt_reason_or_None).

    `in_record` maps a 0-based line index to that line's record segments; see
    record_opener_findings / record_at.

    Backticked `path:line` claims are taken everywhere. On the part of a line
    that is judged at a record's rev, UNWRAPPED `path:line` tokens are taken
    too (UNWRAPPED_CLAIM_RE), so a citation in a table cell or in prose inside
    a record cannot escape the check by omitting backticks."""
    claims = []
    lines = text.split("\n")
    in_record = in_record or {}

    def add(idx, line, start, end, path, cited, exempt, rec):
        quote = find_quote(line, end)
        claims.append(
            {
                "doc": doc_rel,
                "doc_line": idx + 1,
                "path": path,
                "cited_line": cited,
                "quote": quote,
                "exempt_reason": exempt,
                "in_record": rec is not None,
                "record_rev": rec,
            }
        )

    for idx, line in enumerate(lines):
        exempt = None
        if idx > 0:
            m = EXEMPT_RE.search(lines[idx - 1])
            if m:
                exempt = m.group("reason")
        segments = in_record.get(idx)
        masked = line
        for cm in CLAIM_RE.finditer(line):
            add(
                idx, line, cm.start(), cm.end(), cm.group("path"),
                int(cm.group("line")), exempt, record_at(segments, cm.start()),
            )
            # Blank the backticked claim so the unwrapped pass below does not
            # take the same citation a second time.
            masked = (
                masked[: cm.start()] + " " * (cm.end() - cm.start()) + masked[cm.end() :]
            )
        if not segments:
            continue
        for um in UNWRAPPED_CLAIM_RE.finditer(masked):
            rec = record_at(segments, um.start())
            if rec is None:
                # Before the line's first inline record marker: outside the
                # record, so the backticked-only scope applies.
                continue
            add(
                idx, line, um.start(), um.end(), um.group("path"),
                int(um.group("line")), exempt, rec,
            )
    return claims


def check_record_claim(source, reader: RevReader, claim: dict) -> dict | None:
    """A record citation, judged against the record's commit."""
    rev, sha = claim["record_rev"]
    if source.escapes_repo(claim["path"]):
        return finding(
            claim,
            KIND_PATH_ESCAPES_REPO,
            "cited path resolves outside the repository root",
        )
    src_lines = reader.lines(sha, claim["path"])
    if src_lines is None:
        return finding(claim, KIND_PATH_NOT_FOUND, f"cited path does not exist at record rev {rev}")
    if not (1 <= claim["cited_line"] <= len(src_lines)):
        return finding(
            claim,
            KIND_LINE_OUT_OF_RANGE,
            f"file has {len(src_lines)} lines at record rev {rev}, "
            f"claim cites {claim['cited_line']}",
        )
    quote = claim["quote"]
    if quote is None:
        return None
    if norm_ws(quote) in norm_ws(src_lines[claim["cited_line"] - 1]):
        return None
    return finding(
        claim,
        KIND_QUOTE_NOT_FOUND,
        f"quote is not at the cited line at record rev {rev}",
    )


def check_claim(source, claim: dict, reader: RevReader | None = None) -> dict | None:
    if claim.get("record_rev") is not None:
        if reader is None:
            raise Undetermined("a record citation was judged without a rev reader")
        return check_record_claim(source, reader, claim)
    if source.escapes_repo(claim["path"]):
        return finding(
            claim,
            KIND_PATH_ESCAPES_REPO,
            "cited path resolves outside the repository root",
        )

    if source.classify(claim["path"]) != PRESENT_FILE:
        return finding(
            claim,
            KIND_PATH_NOT_FOUND,
            f"cited path does not exist ({source.absent_detail})",
        )

    src = source.read(claim["path"], "cited file")
    src_lines = source_lines(src)

    if not (1 <= claim["cited_line"] <= len(src_lines)):
        return finding(
            claim,
            KIND_LINE_OUT_OF_RANGE,
            f"file has {len(src_lines)} lines, claim cites {claim['cited_line']}",
        )

    quote = claim["quote"]
    if quote is None:
        return None

    needle = norm_ws(quote)
    hits = [i + 1 for i, l in enumerate(src_lines) if needle in norm_ws(l)]

    if not hits:
        # A quote may legitimately span several source lines; fall back to the
        # whole-file view. Found this way we cannot pin a line, so drift is not
        # asserted rather than guessed at.
        if needle in norm_ws(src):
            return None
        return finding(claim, KIND_QUOTE_NOT_FOUND, "quote occurs nowhere in the file")

    if any(abs(h - claim["cited_line"]) <= LINE_DRIFT_TOLERANCE for h in hits):
        return None
    return finding(
        claim,
        KIND_LINE_DRIFTED,
        "quote is at line(s) {} but the claim cites {}".format(
            ", ".join(str(h) for h in hits), claim["cited_line"]
        ),
    )


def finding(claim: dict, kind: str, detail: str) -> dict:
    return {
        "doc": claim["doc"],
        "doc_line": claim["doc_line"],
        "path": claim["path"],
        "cited_line": claim["cited_line"],
        "kind": kind,
        "detail": detail,
        "exempt": claim["exempt_reason"] is not None,
    }


def doc_set(repo: str, explicit: list, source) -> list:
    if explicit:
        out = []
        for rel in explicit:
            if not source.has_doc(rel):
                # The caller asserted this document exists; if it does not, we
                # cannot report on it, and reporting "clean" would be a lie.
                raise Undetermined(
                    f"--doc {rel} is not present in the {source.name} "
                    f"({source.absent_detail}) under {repo}"
                )
            out.append(rel)
        return out
    # RECURSIVE (`docs/**/*.md`), deliberately widened from the flat `docs/*.md`
    # this shipped with. Under a flat glob, moving a document into a
    # subdirectory removes it from coverage with no signal at all: the gate
    # keeps reporting clean over a shrinking scope. That is the same shape as a
    # scan failure collapsing to the empty set, and it is invisible precisely
    # because nothing fails.
    #
    # The scope comes from the SAME source as the content, so in index mode it
    # is the documents the commit records. An unstaged document is not in the
    # commit and is not scanned; a staged one deleted from the working tree is.
    #
    # CLAUDE.md is deliberately NOT added here — it is an instruction/config
    # file, not a documentation page, and its claims are verified by the
    # dedicated scripts/check-claudemd-claims.py gate, which reuses this same
    # engine. Folding it into this gate's default scope would re-blur the two
    # concerns this split exists to separate.
    found = source.doc_rels()
    if not found:
        # An empty scope is NOT a clean scope. Reporting exit 0 here would mean
        # "every claim checks out" on the strength of having read nothing --
        # and it would stay green forever if the documents were renamed or
        # moved out from under the gate. Checking nothing is undetermined.
        raise Undetermined(
            f"no documents to check in the {source.name} of {repo} "
            "(no docs/**/*.md) — an empty scope is not a clean scope"
        )
    return found


def scan(repo: str, explicit_docs: list, source) -> list:
    """Every finding across the doc set, read from `source` on BOTH sides.

    `source` is required and has no default on purpose: a default would be a
    silent choice of which tree gets judged, and that choice is the whole point
    of this gate's contract.
    """
    if not os.path.isdir(repo):
        raise Undetermined(f"not a directory: {repo}")

    rels = doc_set(repo, explicit_docs, source)
    source.prefetch(rels)

    reader = RevReader(repo)
    claims = []
    record_problems = []
    for rel in rels:
        text = source.read(rel, "document")
        problems, inside = record_opener_findings(rel, text.split("\n"), reader)
        record_problems.extend(problems)
        claims.extend(extract_claims(rel, text, inside))

    # Warm the cited-file side too, for the paths that are actually going to be
    # read. A claim that escapes the repo, or names a path the judged tree does
    # not contain, is answered without reading anything. This loop DECIDES
    # NOTHING: an Undetermined raised while classifying here is dropped, because
    # check_claim() below reaches the identical branch a moment later and raises
    # it there, with that claim's own context attached.
    cited = []
    for claim in claims:
        if claim.get("record_rev") is not None:
            continue
        try:
            if not source.escapes_repo(claim["path"]) and (
                source.classify(claim["path"]) == PRESENT_FILE
            ):
                cited.append(claim["path"])
        except Undetermined:
            continue
    source.prefetch(cited)

    results = list(record_problems)
    for claim in claims:
        f = check_claim(source, claim, reader)
        if f is not None:
            results.append(f)
    return results


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--repo", default=None)
    ap.add_argument("--doc", action="append", default=[])
    ap.add_argument("--json", action="store_true")
    ap.add_argument(
        "--source",
        choices=SOURCES,
        default=SOURCE_INDEX,
        help=(
            "which tree to judge, on BOTH the document and the cited-file side. "
            "index (default) is the tree the commit would record; worktree is "
            "the files as they sit on disk, for ad-hoc use."
        ),
    )
    args = ap.parse_args(argv)

    repo = args.repo or os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

    try:
        source = make_source(repo, args.source)
        findings = scan(repo, args.doc, source)
    except Undetermined as exc:
        print(f"check-doc-claims: undetermined — {exc}", file=sys.stderr)
        if args.json:
            print(
                json.dumps(
                    {
                        "verdict": "undetermined",
                        "source": args.source,
                        "findings": [],
                    }
                )
            )
        return 2

    blocking = [f for f in findings if not f["exempt"]]
    verdict = "mismatched" if blocking else "clean"

    if args.json:
        print(
            json.dumps(
                {"verdict": verdict, "source": source.name, "findings": findings}
            )
        )
    else:
        for f in findings:
            mark = "exempt" if f["exempt"] else "BLOCK"
            print(
                "[{}] {}:{} -> {}:{} {} ({})".format(
                    mark,
                    f["doc"],
                    f["doc_line"],
                    f["path"],
                    f["cited_line"],
                    f["kind"],
                    f["detail"],
                )
            )
        if blocking:
            # The remedy differs by source, and printing the index advice under
            # `--source worktree` would be a docstring that lies about its own
            # run -- the exact failure this gate exists to catch.
            if source.name == SOURCE_INDEX:
                remedy = (
                    "Fix the document (or the code) AND STAGE THE FIX — this\n"
                    "gate judges the git index, i.e. the tree the commit would\n"
                    "record, so an unstaged correction does not count (and, by\n"
                    "the same rule, a peer session's unstaged edit cannot block\n"
                    "you)."
                )
            else:
                remedy = (
                    "Fix the document (or the code) on disk. NOTE: you ran\n"
                    "`--source worktree`, so this verdict is about the files as\n"
                    "they sit on disk, NOT about the tree a commit would record.\n"
                    "The pre-commit gate judges the index; re-run without\n"
                    "`--source` to see the verdict that will actually block."
                )
            print(
                "\ncheck-doc-claims: {} claim(s) no longer match the {}.\n"
                "{}\n"
                "If a claim is deliberately historical, say so on the line\n"
                "before it:\n"
                "  <!-- doc-claim-exempt: <reason> -->".format(
                    len(blocking), source.name, remedy
                ),
                file=sys.stderr,
            )
        else:
            print(f"check-doc-claims: all cited claims match the {source.name}.")

    return 1 if blocking else 0


if __name__ == "__main__":
    sys.exit(main())
