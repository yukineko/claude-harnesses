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

Claim syntax
------------
A claim is a backtick-quoted `<path>:<line>` token. Any verbatim quote that
follows it ON THE SAME LINE, delimited by `「 」`, `" "` or backticks, is taken
as part of the claim. Write claims that way and they stay checkable; write them
as loose prose and this gate cannot help you.

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

Note the deliberate asymmetry: a cited file that is MISSING is an answer about
the claim (`path-not-found`, exit 1); a cited file that EXISTS BUT CANNOT BE
READ is not an answer at all (exit 2). Collapsing the second into the first
would let an unreadable tree read as a documented one.

The default scope is `docs/**/*.md`, walked RECURSIVELY, and a scope that comes
out EMPTY is exit 2 rather than exit 0. Both halves guard the same failure: a
gate whose scope silently shrinks to nothing goes on reporting clean, and does
so most convincingly at the moment it has stopped checking anything.
CLAUDE.md is deliberately NOT in this default scope — see the dedicated
check-claudemd-claims.py gate above.

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
import re
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
QUOTE_RE = re.compile(
    r"「(?P<jp>[^」]+)」|\"(?P<dq>[^\"]+)\"|`(?P<bt>[^`]*\s[^`]*)`"
)

EXEMPT_RE = re.compile(r"<!--\s*doc-claim-exempt\s*:\s*(?P<reason>\S.*?)\s*-->")

LINE_DRIFT_TOLERANCE = 10

KIND_PATH_ESCAPES_REPO = "path-escapes-repo"
KIND_PATH_NOT_FOUND = "path-not-found"
KIND_LINE_OUT_OF_RANGE = "line-out-of-range"
KIND_QUOTE_NOT_FOUND = "quote-not-found"
KIND_LINE_DRIFTED = "line-drifted"


class Undetermined(Exception):
    """The verdict could not be established. Always resolves to exit 2."""


def norm_ws(s: str) -> str:
    return " ".join(s.split())


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


def read_text(path: str, what: str) -> str:
    """Read a file, treating every failure as undetermined.

    Callers must have established that the file EXISTS before calling: a
    missing file is a finding about the claim, an unreadable one is a failure
    to observe, and the two must not share a branch.
    """
    try:
        with open(path, "r", encoding="utf-8") as fh:
            return fh.read()
    except (OSError, UnicodeDecodeError) as exc:
        raise Undetermined(f"{what} {path} could not be read: {exc}") from exc


def extract_claims(doc_rel: str, text: str) -> list:
    """(doc, doc_line, path, cited_line, quote_or_None, exempt_reason_or_None)."""
    claims = []
    lines = text.split("\n")
    for idx, line in enumerate(lines):
        exempt = None
        if idx > 0:
            m = EXEMPT_RE.search(lines[idx - 1])
            if m:
                exempt = m.group("reason")
        for cm in CLAIM_RE.finditer(line):
            rest = line[cm.end() :]
            qm = QUOTE_RE.search(rest)
            quote = None
            if qm:
                quote = qm.group("jp") or qm.group("dq") or qm.group("bt")
            claims.append(
                {
                    "doc": doc_rel,
                    "doc_line": idx + 1,
                    "path": cm.group("path"),
                    "cited_line": int(cm.group("line")),
                    "quote": quote,
                    "exempt_reason": exempt,
                }
            )
    return claims


def check_claim(repo: str, claim: dict) -> dict | None:
    target = os.path.join(repo, claim["path"])

    # A claim that resolves outside the repository is not checkable, whichever
    # way it happens to come out. Its verdict would depend on the machine --
    # green on the runner, red on a laptop, or the reverse -- and a verdict
    # that changes with the filesystem around the repo is not a fact about the
    # repo. It is also an unnecessary read primitive: doc text arrives with the
    # diff, so `../..`-style claims would let a document decide what the gate
    # opens and whether the surrounding line matched.
    if os.path.realpath(target) != os.path.realpath(repo) and not os.path.realpath(
        target
    ).startswith(os.path.realpath(repo) + os.sep):
        return finding(
            claim,
            KIND_PATH_ESCAPES_REPO,
            "cited path resolves outside the repository root",
        )

    if not os.path.isfile(target):
        return finding(claim, KIND_PATH_NOT_FOUND, "cited path does not exist")

    src = read_text(target, "cited file")
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


def doc_set(repo: str, explicit: list) -> list:
    if explicit:
        out = []
        for rel in explicit:
            full = os.path.join(repo, rel)
            if not os.path.isfile(full):
                # The caller asserted this document exists; if it does not, we
                # cannot report on it, and reporting "clean" would be a lie.
                raise Undetermined(f"--doc {rel} does not exist under {repo}")
            out.append(rel)
        return out
    found = []
    # RECURSIVE (`docs/**/*.md`), deliberately widened from the flat `docs/*.md`
    # this shipped with. Under a flat glob, moving a document into a
    # subdirectory removes it from coverage with no signal at all: the gate
    # keeps reporting clean over a shrinking scope. That is the same shape as a
    # scan failure collapsing to the empty set, and it is invisible precisely
    # because nothing fails.
    #
    # CLAUDE.md is deliberately NOT added here — it is an instruction/config
    # file, not a documentation page, and its claims are verified by the
    # dedicated scripts/check-claudemd-claims.py gate, which reuses this same
    # engine. Folding it into this gate's default scope would re-blur the two
    # concerns this split exists to separate.
    pattern = os.path.join(repo, "docs", "**", "*.md")
    for full in sorted(glob.glob(pattern, recursive=True)):
        found.append(os.path.relpath(full, repo))
    if not found:
        # An empty scope is NOT a clean scope. Reporting exit 0 here would mean
        # "every claim checks out" on the strength of having read nothing --
        # and it would stay green forever if the documents were renamed or
        # moved out from under the gate. Checking nothing is undetermined.
        raise Undetermined(
            f"no documents to check under {repo} "
            "(no docs/**/*.md) — an empty scope is not a clean scope"
        )
    return found


def scan(repo: str, explicit_docs: list) -> list:
    if not os.path.isdir(repo):
        raise Undetermined(f"not a directory: {repo}")
    results = []
    for rel in doc_set(repo, explicit_docs):
        text = read_text(os.path.join(repo, rel), "document")
        for claim in extract_claims(rel, text):
            f = check_claim(repo, claim)
            if f is not None:
                results.append(f)
    return results


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--repo", default=None)
    ap.add_argument("--doc", action="append", default=[])
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args(argv)

    repo = args.repo or os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

    try:
        findings = scan(repo, args.doc)
    except Undetermined as exc:
        print(f"check-doc-claims: undetermined — {exc}", file=sys.stderr)
        if args.json:
            print(json.dumps({"verdict": "undetermined", "findings": []}))
        return 2

    blocking = [f for f in findings if not f["exempt"]]
    verdict = "mismatched" if blocking else "clean"

    if args.json:
        print(json.dumps({"verdict": verdict, "findings": findings}))
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
            print(
                "\ncheck-doc-claims: {} claim(s) no longer match the code.\n"
                "Fix the document (or the code). If a claim is deliberately\n"
                "historical, say so on the line before it:\n"
                "  <!-- doc-claim-exempt: <reason> -->".format(len(blocking)),
                file=sys.stderr,
            )
        else:
            print("check-doc-claims: all cited claims match the tree.")

    return 1 if blocking else 0


if __name__ == "__main__":
    sys.exit(main())
