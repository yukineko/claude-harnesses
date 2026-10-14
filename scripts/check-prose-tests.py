#!/usr/bin/env python3
"""Block prose claims that no test reproduces.

User ruling 2026-10-15: 「残すべきはテストで、テストのない文書は戯言と一緒。禁忌」,
「指示の文はのこす、事実はテストで再現させる」, 「再現できないものは存在しないものである」,
「記録は過去の失敗であり価値がある、ただしそれは記録である」.

Every unit (sentence, heading, table row) of the covered prose files falls into
exactly one of four classes, decided deterministically and in this order:

  cited        carries `[test: <path>::<name>]` / `[test: <path>]`; every citation
               must resolve to an existing test, or it is `dangling-citation`
  record       begins with a dated marker (`[record: YYYY-MM-DD]`, `記録 YYYY-MM-DD`,
               `実測 YYYY-MM-DD`) or lies in a `<!-- record: YYYY-MM-DD -->` ..
               `<!-- /record -->` block -- a past observation, kept as a record
  imperative   an instruction to the agent: English first word in IMPERATIVE_EN,
               or Japanese deontic ending in IMPERATIVE_JA_ENDINGS
  claim        everything else -> `uncited-claim`

The classifier is conservative: a unit it does not positively recognise as an
instruction or a dated record is a claim.

Covered files: CLAUDE.md, docs/**/*.md, crates/*/README*.md,
crates/*/skills/*/SKILL.md.

Scope: by default the covered files changed in the WORKING TREE vs HEAD
(`git diff HEAD`), since this gate reads file content from disk and the diff
source must match the scanned content. `--all` scans every covered file.

Exit codes (fail-closed):
  0  no findings
  1  at least one finding
  2  undetermined: git failed, a covered/cited file exists but cannot be read
     or decoded, or `--all` found nothing to scan. Takes precedence over 1.
"""

from __future__ import annotations

import argparse
import datetime
import fnmatch
import glob
import os
import posixpath
import re
import subprocess
import sys


class Undetermined(Exception):
    pass


COVERED_GLOBS = (
    "CLAUDE.md",
    "docs/**/*.md",
    "crates/*/README*.md",
    "crates/*/skills/*/SKILL.md",
)

CITATION_RE = re.compile(r"\[test:\s*([^\]\s]+?)\s*\]")
DATE_RE = r"(\d{4})-(\d{2})-(\d{2})"
RECORD_START_RE = re.compile(
    r"^(?:\[record:\s*" + DATE_RE + r"\]"
    r"|記録[:：]?\s*" + DATE_RE +
    r"|実測[:：（(]?\s*" + DATE_RE + r")"
)
RECORD_OPEN_RE = re.compile(r"^\s*<!--\s*record:\s*(\S*)\s*-->\s*$")
RECORD_CLOSE_RE = re.compile(r"^\s*<!--\s*/record\s*-->\s*$")

IMPERATIVE_EN = frozenset(
    """
    do don't never always run use read write add remove delete keep stage
    commit check ask stop avoid prefer call pass set see make put quote report
    return treat verify build install enable disable open edit fix file record
    declare note
    """.split()
)
IMPERATIVE_JA_ENDINGS = (
    "せよ", "しろ", "こと", "ならない", "いけない", "禁止", "禁止する",
    "禁止とする", "ください", "べき", "べきだ", "べきである", "必須",
)
TRAILING_STRIP = "。．.!！）)」』*_ \t"

LIST_MARK_RE = re.compile(r"^\s*(?:[-*+]|\d+[.)])\s+")
HEADING_RE = re.compile(r"^\s{0,3}#{1,6}\s")
FENCE_RE = re.compile(r"^\s*(```|~~~)")
HR_RE = re.compile(r"^\s{0,3}([-*_])(\s*\1){2,}\s*$")
TABLE_SEP_RE = re.compile(r"^\s*\|?\s*:?-{2,}:?\s*(\|\s*:?-{2,}:?\s*)*\|?\s*$")
SENT_SPLIT_RE = re.compile(r"(?<=。)|(?<=[.!?])\s+")
WORDISH_RE = re.compile(r"[0-9A-Za-z぀-ヿ㐀-鿿０-ｚ]")


def valid_date(y: str, m: str, d: str) -> bool:
    try:
        datetime.date(int(y), int(m), int(d))
        return True
    except ValueError:
        return False


# ---------------------------------------------------------------- git / files


def run_git(repo: str, *args: str) -> bytes:
    try:
        proc = subprocess.run(["git", "-C", repo, *args], capture_output=True)
    except OSError as exc:
        raise Undetermined(f"could not run git {' '.join(args)}: {exc}") from exc
    if proc.returncode != 0:
        err = proc.stderr.decode("utf-8", "replace").strip() or "(no stderr)"
        raise Undetermined(
            f"`git {' '.join(args)}` exited {proc.returncode} in {repo}: {err}"
        )
    return proc.stdout


def is_covered(rel: str) -> bool:
    rel = posixpath.normpath(rel)
    if rel == "CLAUDE.md":
        return True
    parts = rel.split("/")
    if parts[0] == "docs" and rel.endswith(".md") and len(parts) >= 2:
        return True
    if len(parts) == 3 and parts[0] == "crates" and fnmatch.fnmatch(parts[2], "README*.md"):
        return True
    if (
        len(parts) == 5
        and parts[0] == "crates"
        and parts[2] == "skills"
        and parts[4] == "SKILL.md"
    ):
        return True
    return False


def all_covered(repo: str) -> list:
    out = set()
    for pat in COVERED_GLOBS:
        for full in glob.glob(os.path.join(repo, pat), recursive=True):
            if os.path.isfile(full):
                rel = os.path.relpath(full, repo).replace(os.sep, "/")
                if is_covered(rel):
                    out.add(rel)
    return sorted(out)


def changed_covered(repo: str) -> list:
    raw = run_git(repo, "diff", "HEAD", "--name-only", "-z", "--diff-filter=d")
    rels = []
    for item in raw.split(b"\0"):
        if not item:
            continue
        rel = item.decode("utf-8", "surrogateescape")
        if is_covered(rel):
            rels.append(rel)
    return sorted(set(rels))


def read_text(path: str, what: str) -> str:
    try:
        with open(path, "rb") as fh:
            data = fh.read()
    except OSError as exc:
        raise Undetermined(f"{what} {path} could not be read: {exc}") from exc
    try:
        return data.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise Undetermined(f"{what} {path} is not valid UTF-8: {exc}") from exc


# ---------------------------------------------------------------- citations


def escapes(rel: str) -> bool:
    if not rel or posixpath.isabs(rel):
        return True
    n = posixpath.normpath(rel)
    return n == ".." or n.startswith("../")


def rs_has_test_fn(text: str, name: str) -> bool:
    lines = text.split("\n")
    fn_re = re.compile(r"\bfn\s+" + re.escape(name) + r"\b")
    for i, line in enumerate(lines):
        if not fn_re.search(line):
            continue
        j = i - 1
        while j >= 0:
            s = lines[j].strip()
            if not s or s.startswith("//"):
                j -= 1
                continue
            if s.startswith("#["):
                if re.search(r"\btest\b", s):
                    return True
                j -= 1
                continue
            break
    return False


def resolve_citation(repo: str, target: str, cache: dict):
    """Return None when the citation resolves, else a reason string."""
    if "::" in target:
        path, name = target.split("::", 1)
    else:
        path, name = target, ""
    if escapes(path):
        return "path escapes the repository"
    full = os.path.join(repo, path)
    real_root = os.path.realpath(repo)
    real = os.path.realpath(full)
    if real != real_root and not real.startswith(real_root + os.sep):
        return "path escapes the repository"
    if not os.path.isfile(full):
        return "no such file"
    if full not in cache:
        cache[full] = read_text(full, "cited test file")
    text = cache[full]
    base = posixpath.basename(path)
    if path.endswith(".rs"):
        if not name:
            return "a Rust citation must name the test fn (path::fn)"
        if not rs_has_test_fn(text, name):
            return f"no #[test]-attributed fn {name} in {path}"
        return None
    if path.endswith(".py"):
        if not name:
            return "a Python citation must name the test (path::test_fn)"
        last = name.split("::")[-1]
        if not last.startswith("test"):
            return f"{last} is not a test name (must start with 'test')"
        if not re.search(r"\bdef\s+" + re.escape(last) + r"\s*\(", text):
            return f"no def {last}( in {path}"
        return None
    if "tests" not in path.split("/")[:-1] and not base.startswith("test"):
        return "not a test file (needs a tests/ directory or a test* basename)"
    if name and name not in text:
        return f"case {name!r} not found in {path}"
    return None


# ---------------------------------------------------------------- units


def strip_lead(s: str) -> str:
    s = s.strip()
    while True:
        prev = s
        s = re.sub(r"^>\s*", "", s)
        s = LIST_MARK_RE.sub("", s)
        s = re.sub(r"^\[[ xX]\]\s+", "", s)
        s = re.sub(r"^#{1,6}\s+", "", s)
        s = s.lstrip("*_ \t")
        if s == prev:
            return s


def has_wordish(s: str) -> bool:
    s = re.sub(r"\]\([^)]*\)", "]", s)
    s = re.sub(r"`[^`]*`", "", s)
    s = CITATION_RE.sub("", s)
    return bool(WORDISH_RE.search(s))


def is_record_unit(s: str) -> bool:
    t = strip_lead(s).lstrip("(（").lstrip()
    m = RECORD_START_RE.match(t)
    if not m:
        return False
    g = [x for x in m.groups() if x is not None]
    return len(g) == 3 and valid_date(*g)


def is_imperative(s: str) -> bool:
    t = strip_lead(s)
    first = re.match(r"[A-Za-z][A-Za-z']*", t)
    if first and first.group(0).lower() in IMPERATIVE_EN:
        return True
    tail = t.rstrip(TRAILING_STRIP)
    return any(tail.endswith(e) for e in IMPERATIVE_JA_ENDINGS)


def iter_units(text: str):
    """Yield (line_no, unit_text, in_record_block) and malformed-block findings.

    Yields tuples ("unit", line, text, in_record) or ("malformed", line, msg).
    """
    lines = text.replace("\r\n", "\n").replace("\r", "\n").split("\n")
    i = 0
    n = len(lines)
    # front matter
    if n and lines[0].strip() == "---":
        j = 1
        while j < n and lines[j].strip() != "---":
            j += 1
        if j < n:
            k = 1
            while k < j:
                m = re.match(r"^description:\s*(.*)$", lines[k])
                if m:
                    val = m.group(1).strip()
                    start = k + 1
                    k += 1
                    if val in ("|", ">", "|-", ">-", ""):
                        val = ""
                    while k < j and (lines[k].startswith((" ", "\t")) or not lines[k].strip()):
                        val += " " + lines[k].strip()
                        k += 1
                    val = val.strip().strip("'\"")
                    if val:
                        yield ("para", start, val, False)
                    continue
                k += 1
            i = j + 1

    in_fence = None
    in_comment = False
    record = None  # line of open record block
    para = []  # list of (line_no, text)

    def flush():
        if para:
            yield ("para", para[0][0], para, record is not None)

    while i < n:
        raw = lines[i]
        ln = i + 1
        i += 1
        if in_fence:
            if raw.strip().startswith(in_fence):
                in_fence = None
            continue
        if in_comment:
            if "-->" in raw:
                in_comment = False
            continue
        if RECORD_OPEN_RE.match(raw):
            yield from flush()
            para = []
            if record is not None:
                yield ("malformed", ln, "record block opened inside another record block")
            dm = re.fullmatch(DATE_RE, RECORD_OPEN_RE.match(raw).group(1))
            if not dm or not valid_date(*dm.groups()):
                yield ("malformed", ln, "record block opener needs a valid YYYY-MM-DD date")
            record = ln
            continue
        if RECORD_CLOSE_RE.match(raw):
            yield from flush()
            para = []
            if record is None:
                yield ("malformed", ln, "record block close without an opener")
            record = None
            continue
        fm = FENCE_RE.match(raw)
        if fm:
            yield from flush()
            para = []
            in_fence = fm.group(1)
            continue
        stripped = raw.strip()
        if stripped.startswith("<!--"):
            yield from flush()
            para = []
            if "-->" not in stripped:
                in_comment = True
            continue
        if not stripped or HR_RE.match(raw) or TABLE_SEP_RE.match(raw):
            yield from flush()
            para = []
            continue
        if HEADING_RE.match(raw) or stripped.startswith("|"):
            yield from flush()
            para = []
            yield ("single", ln, stripped, record is not None)
            continue
        new_block = bool(LIST_MARK_RE.match(raw)) or stripped.startswith(">")
        if new_block:
            yield from flush()
            para = [(ln, stripped)]
            continue
        para.append((ln, stripped))
    yield from flush()
    if record is not None:
        yield ("malformed", record, "record block is never closed (<!-- /record -->)")


def split_sentences(para):
    """para: list of (line_no, text) -> list of (line_no, sentence)."""
    out = []
    for ln, text in para:
        pieces = [p for p in SENT_SPLIT_RE.split(text) if p is not None]
        for idx, p in enumerate(pieces):
            if idx == 0 and out and not out[-1][2]:
                # continuation of an unterminated sentence from the previous line
                out[-1] = (out[-1][0], out[-1][1] + " " + p, False)
            else:
                out.append((ln, p, False))
            ended = bool(re.search(r"[。.!?]\s*$", out[-1][1]))
            out[-1] = (out[-1][0], out[-1][1], ended)
    # A fragment that is ONLY citations (`Claim. [test: a::b]`) belongs to the
    # sentence before it: the citation is written after that sentence's stop.
    merged = []
    for ln, s, _ in out:
        s = s.strip()
        if not s:
            continue
        if merged and CITATION_RE.search(s) and not CITATION_RE.sub("", s).strip(" \t.,;。"):
            merged[-1] = (merged[-1][0], merged[-1][1] + " " + s)
        else:
            merged.append((ln, s))
    return merged


def check_file(repo: str, rel: str, cache: dict) -> list:
    full = os.path.join(repo, rel)
    text = read_text(full, "covered file")
    findings = []
    for ev in iter_units(text):
        if ev[0] == "malformed":
            findings.append(f"{rel}:{ev[1]}: malformed-record-block: {ev[2]}")
            continue
        kind, ln, payload, in_record = ev
        if kind == "single":
            units = [(ln, payload)]
        elif isinstance(payload, str):
            units = split_sentences([(ln, payload)])
        else:
            units = split_sentences(payload)
        for uln, unit in units:
            if not has_wordish(unit) and not CITATION_RE.search(unit):
                continue
            cites = CITATION_RE.findall(unit)
            if cites:
                for c in cites:
                    reason = resolve_citation(repo, c, cache)
                    if reason:
                        findings.append(
                            f"{rel}:{uln}: dangling-citation: [test: {c}] ({reason})"
                        )
                continue
            if in_record or is_record_unit(unit):
                continue
            if is_imperative(unit):
                continue
            findings.append(f"{rel}:{uln}: uncited-claim: {unit}")
    return findings


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--repo")
    ap.add_argument("--all", action="store_true")
    ap.add_argument("files", nargs="*")
    args = ap.parse_args(argv)
    try:
        if args.repo:
            repo = os.path.abspath(args.repo)
        else:
            repo = run_git(os.getcwd(), "rev-parse", "--show-toplevel").decode().strip()
        if args.files:
            rels = sorted({posixpath.normpath(f) for f in args.files if is_covered(f)})
        elif args.all:
            rels = all_covered(repo)
            if not rels:
                raise Undetermined(f"--all found no covered file under {repo}")
        else:
            rels = changed_covered(repo)
        cache: dict = {}
        findings = []
        for rel in rels:
            findings.extend(check_file(repo, rel, cache))
    except Undetermined as exc:
        print(f"check-prose-tests: UNDETERMINED: {exc}", file=sys.stderr)
        return 2
    for f in findings:
        print(f)
    if findings:
        print(
            f"\ncheck-prose-tests: {len(findings)} finding(s) in {len(rels)} file(s). "
            "A claim needs `[test: <path>::<name>]` pointing at a test that "
            "reproduces it; a record needs a dated marker (`実測 YYYY-MM-DD`, "
            "`[record: YYYY-MM-DD]`); a claim no test can reproduce is deleted.",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
