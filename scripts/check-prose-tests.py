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
               `実測 YYYY-MM-DD`, each optionally followed by `rev: <sha>`) or lies
               in a `<!-- record: YYYY-MM-DD rev: <sha> -->` .. `<!-- /record -->`
               block -- a past observation, kept as a record. A block opener
               MUST carry `rev:`; any `rev:` given must resolve to a commit in
               the repo, else the record is reported and not exempt
  imperative   an instruction to the agent: English first word in IMPERATIVE_EN,
               or Japanese deontic/prohibitive ending in IMPERATIVE_JA_ENDINGS
               (incl. しない); under docs/specs/ also a must/shall/
               しなければならない requirement. EXEMPT ONLY IF it names no
               concrete mechanism (MECHANISM_RES: code span, ENV_VAR, file
               name, path, --flag, /command, hook event name); an imperative
               naming one is treated as a claim (user ruling 2026-10-15)
  claim        everything else -> `uncited-claim`

One structural exemption: a heading declared in MACHINE_ANCHOR_HEADINGS (exact
file + exact heading line, with the gate script that parses it as an anchor)
is not a unit at all.

Comments inside fenced code blocks (`#` / `//` by the fence's language, both
for a bare fence) are units too; code outside comments never is.

The classifier is conservative: a unit it does not positively recognise as an
instruction or a dated record is a claim.

Covered files: CLAUDE.md, docs/**/*.md, crates/*/README*.md,
crates/*/skills/*/SKILL.md.

Scope: by default only the units that the change ADDS or MODIFIES, i.e. whose
line span intersects the added/modified lines of `git diff HEAD -U0` in the
WORKING TREE (this gate reads content from disk, so the diff source matches the
scanned content); a pure deletion touches the lines around it (user ruling
2026-10-15: the existing corpus is cleaned area by area, not on every touch).
`--all` and explicit FILE arguments check every unit.

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
REV_RE = r"([0-9a-fA-F]{7,40})"
REV_TAIL = r"(?:\s+rev:\s*" + REV_RE + r")?"
RECORD_START_RES = (
    re.compile(r"^\[record:\s*" + DATE_RE + REV_TAIL + r"\s*\]"),
    re.compile(r"^記録[:：]?\s*" + DATE_RE + REV_TAIL),
    re.compile(r"^実測[:：（(]?\s*" + DATE_RE + REV_TAIL),
)
RECORD_OPEN_RE = re.compile(r"^\s*<!--\s*record:\s*(.*?)\s*-->\s*$")
RECORD_OPENER_BODY_RE = re.compile(DATE_RE + r"\s+rev:\s*" + REV_RE)
RECORD_CLOSE_RE = re.compile(r"^\s*<!--\s*/record\s*-->\s*$")

IMPERATIVE_EN = frozenset(
    """
    do don't never always run use read write add remove delete keep stage
    commit check ask avoid prefer call pass set see make put quote report
    return treat verify build install enable disable open edit fix file record
    declare note
    """.split()
)
IMPERATIVE_JA_ENDINGS = (
    "せよ", "しろ", "こと", "ならない", "いけない", "禁止", "禁止する",
    "禁止とする", "ください", "べき", "べきだ", "べきである", "必須",
    "しない",
)
# A1: an imperative that names a concrete mechanism must be cited like a claim.
HOOK_EVENTS = (
    "SessionStart", "SessionEnd", "Stop", "SubagentStop", "PreToolUse",
    "PostToolUse", "UserPromptSubmit", "Notification", "PreCompact",
)
MECHANISM_RES = (
    re.compile(r"`[^`]+`"),
    re.compile(r"(?<![A-Za-z0-9_])[A-Z][A-Z0-9]*_[A-Z0-9_]+"),
    re.compile(r"[\w.-]+\.(?:toml|json|md|rs|py|sh|yaml|yml|lock|txt)\b"),
    re.compile(r"[\w~.]/[\w.]"),
    re.compile(r"--[A-Za-z]"),
    re.compile(r"(?:^|(?<=[\s(（「]))/[a-z][\w:-]*"),
    re.compile(r"(?<![A-Za-z0-9_])(?:" + "|".join(HOOK_EVENTS) + r")(?![A-Za-z0-9_])"),
)
# A2: normative requirement words, docs/specs only.
NORMATIVE_RE = re.compile(r"(?i)(?<![A-Za-z])(?:must|shall)(?![A-Za-z])")
# A3: comment syntax per fence info string ("" = both).
HASH_LANGS = frozenset("sh bash shell zsh console toml yaml yml python py ini conf cfg make makefile dockerfile ruby rb".split())
SLASH_LANGS = frozenset("rust rs js javascript ts typescript c cpp go java jsonc json5 swift kotlin kt scala".split())
TRAILING_STRIP = "。．.!！）)」』*_ \t"

# A heading that a repo gate script parses as a machine anchor is structure,
# not a claim. Exempt ONLY the exact (covered file, stripped heading line) pairs
# declared here, each with the (script, function) that parses it. Any other
# heading -- the same text in another file, a different level, extra words --
# is still a unit. scripts/test_check_prose_tests.py checks that every declared
# parser really depends on its heading in the real repo file.
MACHINE_ANCHOR_HEADINGS = {
    ("crates/overwatch/skills/continuous-audit/SKILL.md", "## 対象 crate (既定)"):
        ("scripts/check-gate-crates-sync.py", "skill_md_crates"),
}

LIST_MARK_RE = re.compile(r"^\s*(?:[-*+]|\d+[.)])\s+")
HEADING_RE = re.compile(r"^\s{0,3}#{1,6}\s")
FENCE_RE = re.compile(r"^\s*(```|~~~)")
HR_RE = re.compile(r"^\s{0,3}([-*_])(\s*\1){2,}\s*$")
TABLE_SEP_RE = re.compile(r"^\s*\|?\s*:?-{2,}:?\s*(\|\s*:?-{2,}:?\s*)*\|?\s*$")
# Split after 。, and after . ! ? + whitespace -- except a digit-then-period
# followed by a non-ASCII character (`3. に従って`: a section reference inside a
# Japanese sentence, not a sentence end).
SENT_SPLIT_RE = re.compile(r"(?<=。)|(?<=[.!?])(?<![0-9]\.)\s+|(?<=[0-9]\.)\s+(?=[\x00-\x7f])")
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


HUNK_RE = re.compile(r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@")


def changed_lines(repo: str, rels: list) -> dict:
    """{rel: set of new-side line numbers added or modified vs HEAD}.

    A pure deletion (`+c,0`) touches lines c and c+1, the units around the
    deletion point. A diff line this parser cannot place is undetermined.
    """
    out = {r: set() for r in rels}
    if not rels:
        return out
    raw = run_git(
        repo, "-c", "core.quotepath=off", "diff", "HEAD", "-U0", "--no-color",
        "--no-ext-diff", "--no-renames", "--", *rels,
    ).decode("utf-8", "surrogateescape")
    cur = None
    for line in raw.split("\n"):
        if line.startswith("+++ "):
            target = line[4:]
            if target == "/dev/null":
                cur = None
                continue
            if not target.startswith("b/"):
                raise Undetermined(f"unparseable diff target line: {line!r}")
            cur = target[2:]
            if cur not in out:
                raise Undetermined(f"diff names a file outside the request: {cur!r}")
            continue
        m = HUNK_RE.match(line)
        if m:
            if cur is None:
                raise Undetermined(f"diff hunk without a target file: {line!r}")
            start = int(m.group(1))
            count = 1 if m.group(2) is None else int(m.group(2))
            if count == 0:
                out[cur].update({start, start + 1})
            else:
                out[cur].update(range(start, start + count))
    return out


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


def record_marker(s: str):
    """None if `s` is not a record unit, else (date_ok, rev_or_None)."""
    t = strip_lead(s).lstrip("(（").lstrip()
    for rx in RECORD_START_RES:
        m = rx.match(t)
        if m:
            y, mo, d, rev = m.groups()
            return (valid_date(y, mo, d), rev)
    return None


def make_rev_checker(repo: str):
    """rev -> True if it names a commit in `repo`, False if it does not.
    A git failure other than 'no such object' is UNDETERMINED (raised)."""
    seen: dict = {}

    def ok(rev: str) -> bool:
        if rev not in seen:
            try:
                proc = subprocess.run(
                    ["git", "-C", repo, "rev-parse", "--verify", "--quiet",
                     "--end-of-options", rev + "^{commit}"],
                    capture_output=True,
                )
            except OSError as exc:
                raise Undetermined(f"could not run git rev-parse: {exc}") from exc
            if proc.returncode == 0:
                seen[rev] = True
            elif proc.returncode == 1 and not proc.stdout.strip():
                seen[rev] = False
            else:
                err = proc.stderr.decode("utf-8", "replace").strip() or "(no stderr)"
                raise Undetermined(
                    f"`git rev-parse --verify {rev}^{{commit}}` exited "
                    f"{proc.returncode} in {repo}: {err}"
                )
        return seen[rev]

    return ok


def names_mechanism(s: str) -> bool:
    s = CITATION_RE.sub("", s)
    s = re.sub(r"\]\([^)]*\)", "]", s)
    return any(r.search(s) for r in MECHANISM_RES)


def is_normative(s: str) -> bool:
    t = strip_lead(s).rstrip(TRAILING_STRIP)
    return bool(NORMATIVE_RE.search(t)) or t.endswith("しなければならない")


def is_imperative(s: str) -> bool:
    t = strip_lead(s)
    first = re.match(r"[A-Za-z][A-Za-z']*", t)
    if first and first.group(0).lower() in IMPERATIVE_EN:
        return True
    tail = t.rstrip(TRAILING_STRIP)
    return any(tail.endswith(e) for e in IMPERATIVE_JA_ENDINGS)


def iter_units(text: str, rev_ok=None):
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
                    pieces = []
                    if val not in ("|", ">", "|-", ">-", ""):
                        pieces.append((k + 1, val.strip("'\"")))
                    k += 1
                    while k < j and (lines[k].startswith((" ", "\t")) or not lines[k].strip()):
                        if lines[k].strip():
                            pieces.append((k + 1, lines[k].strip().strip("'\"")))
                        k += 1
                    if pieces:
                        yield ("para", pieces[0][0], pieces, False)
                    continue
                k += 1
            i = j + 1

    in_fence = None
    in_comment = False
    record = None  # line of open record block
    record_ok = False  # the open block's opener is well-formed (exempts content)
    para = []  # list of (line_no, text)

    def flush():
        if para:
            yield ("para", para[0][0], para, record_ok)

    while i < n:
        raw = lines[i]
        ln = i + 1
        i += 1
        if in_fence:
            if raw.strip().startswith(in_fence[0]):
                in_fence = None
                yield from flush()
                para = []
                continue
            c = code_comment(raw, in_fence[1])
            if c is None:
                yield from flush()
                para = []
            elif c[1]:  # full-line comment: joins consecutive ones
                para.append((ln, c[0]))
            else:
                yield from flush()
                para = []
                yield ("para", ln, [(ln, c[0])], record_ok)
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
            body = RECORD_OPEN_RE.match(raw).group(1)
            dm = RECORD_OPENER_BODY_RE.fullmatch(body)
            record = ln
            record_ok = False
            if not dm:
                yield ("malformed", ln,
                       "record block opener needs `record: YYYY-MM-DD rev: <sha>` "
                       "(the commit at which the record was true)")
            elif not valid_date(*dm.groups()[:3]):
                yield ("malformed", ln, "record block opener needs a valid YYYY-MM-DD date")
            elif rev_ok is None or not rev_ok(dm.group(4)):
                yield ("malformed", ln,
                       f"record block rev {dm.group(4)} does not resolve to a commit")
            else:
                record_ok = True
            continue
        if RECORD_CLOSE_RE.match(raw):
            yield from flush()
            para = []
            if record is None:
                yield ("malformed", ln, "record block close without an opener")
            record = None
            record_ok = False
            continue
        fm = FENCE_RE.match(raw)
        if fm:
            yield from flush()
            para = []
            info = raw.strip()[3:].strip().split()
            lang = info[0].lower() if info else ""
            in_fence = (fm.group(1), lang)
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
            yield ("single", ln, stripped, record_ok)
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


def code_comment(line: str, lang: str):
    """Return (comment_text, is_full_line) for a code line, or None.

    Only the markers of the fence's language are honoured ("" = both). A
    marker inside a '...' or "..." string, or a `#!` shebang, is not a comment.
    """
    markers = []
    if lang in HASH_LANGS or lang == "":
        markers.append("#")
    if lang in SLASH_LANGS or lang == "":
        markers.append("//")
    if not markers:
        return None
    if line.lstrip().startswith("#!"):
        return None
    quote = None
    i = 0
    while i < len(line):
        ch = line[i]
        if quote:
            if ch == "\\":
                i += 2
                continue
            if ch == quote:
                quote = None
        elif ch in ("'", '"'):
            quote = ch
        else:
            for m in markers:
                if line.startswith(m, i) and (i == 0 or line[i - 1].isspace()):
                    text = line[i + len(m):].lstrip("/!# \t").strip()
                    full = not line[:i].strip()
                    return (text, full) if text else None
        i += 1
    return None


def split_sentences(para):
    """para: list of (line_no, text) -> list of (first_line, last_line, sentence)."""
    out = []
    for k, (ln, text) in enumerate(para):
        if k == 0:
            # the list marker (`1. `, `- `) is structure, not a sentence
            text = LIST_MARK_RE.sub("", text, count=1)
        pieces = [p for p in SENT_SPLIT_RE.split(text) if p is not None]
        for idx, p in enumerate(pieces):
            if idx == 0 and out and not out[-1][2]:
                # continuation of an unterminated sentence from the previous line
                out[-1] = (out[-1][0], out[-1][1] + " " + p, False, ln)
            else:
                out.append((ln, p, False, ln))
            ended = bool(re.search(r"[。.!?]\s*$", out[-1][1]))
            out[-1] = (out[-1][0], out[-1][1], ended, out[-1][3])
    # A fragment that is ONLY citations (`Claim. [test: a::b]`) belongs to the
    # sentence before it: the citation is written after that sentence's stop.
    merged = []
    for ln, s, _, end in out:
        s = s.strip()
        if not s:
            continue
        if merged and CITATION_RE.search(s) and not CITATION_RE.sub("", s).strip(" \t.,;。"):
            merged[-1] = (merged[-1][0], end, merged[-1][2] + " " + s)
        else:
            merged.append((ln, end, s))
    return merged


def check_file(repo: str, rel: str, cache: dict, touched=None) -> list:
    """Check one covered file. `touched` is the set of changed new-side line
    numbers (diff-scoped default mode), or None to check every unit."""
    full = os.path.join(repo, rel)
    text = read_text(full, "covered file")
    findings = []
    rev_ok = cache.setdefault("__rev_ok__", make_rev_checker(repo))
    for ev in iter_units(text, rev_ok):
        if ev[0] == "malformed":
            findings.append(f"{rel}:{ev[1]}: malformed-record-block: {ev[2]}")
            continue
        kind, ln, payload, in_record = ev
        if kind == "single":
            if (rel, payload) in MACHINE_ANCHOR_HEADINGS and HEADING_RE.match(payload):
                continue
            units = [(ln, ln, payload)]
        else:
            units = split_sentences(payload)
        for uln, uend, unit in units:
            if touched is not None and not any(
                x in touched for x in range(uln, uend + 1)
            ):
                continue
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
            if in_record:
                continue
            rec = record_marker(unit)
            if rec is not None and rec[0]:
                if rec[1] is None or rev_ok(rec[1]):
                    continue
                findings.append(
                    f"{rel}:{uln}: unresolved-record-rev: rev {rec[1]} does not "
                    f"resolve to a commit: {unit}"
                )
                continue
            imperative = is_imperative(unit) or (
                rel.startswith("docs/specs/") and is_normative(unit)
            )
            if imperative and not names_mechanism(unit):
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
        touched: dict = {}
        if args.files:
            rels = sorted({posixpath.normpath(f) for f in args.files if is_covered(f)})
        elif args.all:
            rels = all_covered(repo)
            if not rels:
                raise Undetermined(f"--all found no covered file under {repo}")
        else:
            rels = changed_covered(repo)
            touched = changed_lines(repo, rels)
        cache: dict = {}
        findings = []
        for rel in rels:
            findings.extend(check_file(repo, rel, cache, touched.get(rel)))
    except Undetermined as exc:
        print(f"check-prose-tests: UNDETERMINED: {exc}", file=sys.stderr)
        return 2
    for f in findings:
        print(f)
    if findings:
        print(
            f"\ncheck-prose-tests: {len(findings)} finding(s) in {len(rels)} file(s). "
            "A claim needs `[test: <path>::<name>]` pointing at a test that "
            "reproduces it; a record needs a dated marker with the commit it was true at "
            "(`[record: YYYY-MM-DD rev: <sha>]`, or a `<!-- record: YYYY-MM-DD rev: <sha> -->` "
            "block); a claim no test can reproduce is deleted.",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
