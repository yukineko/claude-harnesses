#!/usr/bin/env python3
"""Block a commit that adds a copy of an instruction block to a second file.

User request 2026-10-15: 「同じような文書の指示が点在していないか? 機能が無駄に分散している
証拠ではないか?」 -> 「この機能そのものをgateなり監査に実装して」.

The audit that preceded this gate (2026-10-15, rev a946b39f) found that the cost of a
copied instruction is drift: a procedure pasted into a second skill or command keeps
describing the binary as it was when copied, while the binary moves on. The remedy for
a new copy is a pointer to the one canonical place. Duplication that already exists is
grandfathered: this is a ratchet, and only an INCREASE blocks.

Corpus (paths relative to the git toplevel):
  CLAUDE.md, docs/**/*.md, crates/*/README*.md,
  crates/*/skills/**/*.md, crates/*/agents/*.md, crates/*/commands/*.md
INSTRUCTION files are CLAUDE.md and the skills/agents/commands files.

Blocks: a fenced code block is one block, a heading line is one block, and otherwise a
run of non-blank lines is one paragraph block. Normalisation lower-cases, turns runs of
markdown punctuation into a space and deletes all whitespace; blocks shorter than
MIN_CHARS normalised characters are ignored. Similarity is the exact Jaccard index of
the K-character shingle sets. A PAIR is two blocks with similarity >= THRESHOLD, in
different files, and at least one file an instruction file (so README translation
mirrors never pair). Candidate generation uses prefix filtering, which
is exact: no pair at or above the threshold is skipped.

The constants are deliberately not command-line options: there is no knob to loosen.

Default mode: for every corpus file F changed in the working tree vs HEAD and every
other corpus file G, count the pairs between F and G in the working tree and at HEAD.
A count that rose is a finding.
--all: report every pair in the working-tree corpus (the audit).

Exit codes (fail-closed):
  0  no findings
  1  at least one finding
  2  undetermined: not a git repo, git failed (including no HEAD in default mode), a
     corpus file cannot be read or is not UTF-8, or --all found an empty corpus.
     Takes precedence over 1.
"""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
from collections import defaultdict

THRESHOLD = 0.6
MIN_CHARS = 80
K = 5

NAME = "check-doc-duplication"

CORPUS_RES = (
    re.compile(r"^CLAUDE\.md$"),
    re.compile(r"^docs/.+\.md$"),
    re.compile(r"^crates/[^/]+/README[^/]*\.md$"),
    re.compile(r"^crates/[^/]+/skills/.+\.md$"),
    re.compile(r"^crates/[^/]+/agents/[^/]+\.md$"),
    re.compile(r"^crates/[^/]+/commands/[^/]+\.md$"),
)
INSTRUCTION_RES = (
    re.compile(r"^CLAUDE\.md$"),
    re.compile(r"^crates/[^/]+/skills/.+\.md$"),
    re.compile(r"^crates/[^/]+/agents/[^/]+\.md$"),
    re.compile(r"^crates/[^/]+/commands/[^/]+\.md$"),
)
HEADING_RE = re.compile(r"^#{1,6} ")
PUNCT_RE = re.compile(r"[`*_>#|\-\[\]()~:=]+")
SPACE_RE = re.compile(r"\s+")


class Undetermined(Exception):
    pass


def in_corpus(path: str) -> bool:
    return any(r.match(path) for r in CORPUS_RES)


def is_instruction(path: str) -> bool:
    return any(r.match(path) for r in INSTRUCTION_RES)


def pair_allowed(fa: str, fb: str) -> bool:
    if fa == fb:
        return False
    # README.md <-> README.ja.md translation mirrors fall out here: neither side is
    # an instruction file.
    return is_instruction(fa) or is_instruction(fb)


# ---------------------------------------------------------------- git / files


def run_git(repo: str, *args: str, stdin: bytes | None = None) -> bytes:
    try:
        proc = subprocess.run(
            ["git", "-C", repo, *args], input=stdin, capture_output=True
        )
    except OSError as exc:
        raise Undetermined(f"could not run git {args[0]}: {exc}") from exc
    if proc.returncode != 0:
        err = proc.stderr.decode("utf-8", "replace").strip()
        raise Undetermined(f"git {' '.join(args)} exited {proc.returncode}: {err}")
    return proc.stdout


def decode(path: str, data: bytes, where: str) -> str:
    try:
        return data.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise Undetermined(f"{where} {path} is not valid UTF-8: {exc}") from exc


def worktree_files(repo: str) -> list[str]:
    out = run_git(repo, "ls-files", "-z", "--cached", "--others", "--exclude-standard")
    paths = sorted({p for p in out.decode("utf-8", "replace").split("\0") if p})
    return [p for p in paths if in_corpus(p) and os.path.lexists(os.path.join(repo, p))]


def read_worktree(repo: str, paths: list[str]) -> dict[str, str]:
    texts = {}
    for p in paths:
        try:
            with open(os.path.join(repo, p), "rb") as fh:
                data = fh.read()
        except OSError as exc:
            raise Undetermined(f"cannot read {p}: {exc}") from exc
        texts[p] = decode(p, data, "working-tree file")
    return texts


def read_head(repo: str) -> dict[str, str]:
    out = run_git(repo, "ls-tree", "-r", "-z", "HEAD")
    entries = []
    for rec in out.decode("utf-8", "replace").split("\0"):
        if not rec:
            continue
        meta, _, path = rec.partition("\t")
        parts = meta.split()
        if len(parts) != 3:
            raise Undetermined(f"unparseable ls-tree record: {rec!r}")
        if parts[1] == "blob" and in_corpus(path):
            entries.append((parts[2], path))
    if not entries:
        return {}
    stdin = "".join(oid + "\n" for oid, _ in entries).encode()
    data = run_git(repo, "cat-file", "--batch", stdin=stdin)
    texts = {}
    pos = 0
    for oid, path in entries:
        nl = data.find(b"\n", pos)
        if nl < 0:
            raise Undetermined(f"truncated cat-file output at {path}")
        header = data[pos:nl].split()
        if len(header) != 3 or header[1] != b"blob":
            raise Undetermined(f"unexpected cat-file header for {path}: {header!r}")
        size = int(header[2])
        body = data[nl + 1 : nl + 1 + size]
        if len(body) != size:
            raise Undetermined(f"truncated cat-file body for {path}")
        texts[path] = decode(path, body, "HEAD file")
        pos = nl + 1 + size + 1
    return texts


# ---------------------------------------------------------------- blocks


def split_blocks(text: str) -> list[tuple[int, int, str]]:
    out: list[tuple[int, int, str]] = []
    cur: list[str] = []
    start = 0
    infence = False

    def flush(end: int) -> None:
        nonlocal cur, start
        if cur:
            out.append((start, end, "\n".join(cur)))
        cur, start = [], 0

    lines = text.split("\n")
    for i, line in enumerate(lines, 1):
        stripped = line.strip()
        if stripped.startswith("```"):
            if not infence:
                flush(i - 1)
                infence = True
                start, cur = i, [line]
            else:
                cur.append(line)
                infence = False
                flush(i)
            continue
        if infence:
            cur.append(line)
            continue
        if stripped == "":
            flush(i - 1)
            continue
        if HEADING_RE.match(line):
            flush(i - 1)
            start, cur = i, [line]
            flush(i)
            continue
        if not cur:
            start = i
        cur.append(line)
    flush(len(lines))
    return out


def normalise(text: str) -> str:
    return SPACE_RE.sub("", PUNCT_RE.sub(" ", text.lower()))


class Block:
    __slots__ = ("path", "start", "end", "shingles")

    def __init__(self, path: str, start: int, end: int, shingles: frozenset):
        self.path, self.start, self.end, self.shingles = path, start, end, shingles

    def loc(self) -> str:
        return f"{self.path}:{self.start}-{self.end}"


def blocks_of(texts: dict[str, str]) -> list[Block]:
    out = []
    for path in sorted(texts):
        for start, end, text in split_blocks(texts[path]):
            n = normalise(text)
            if len(n) < MIN_CHARS:
                continue
            sh = frozenset(n[i : i + K] for i in range(len(n) - K + 1))
            out.append(Block(path, start, end, sh))
    return out


def find_pairs(blocks: list[Block], probe: set[str] | None):
    """All allowed pairs with Jaccard >= THRESHOLD. If `probe` is given, only pairs
    with at least one block in a file of `probe`. Prefix filtering: under a global
    order, two sets with Jaccard >= t share an element of each other's prefix of
    length |S| - floor(t*|S|) + 1, so probing prefixes loses no qualifying pair."""
    freq: dict[str, int] = defaultdict(int)
    for b in blocks:
        for s in b.shingles:
            freq[s] += 1
    prefixes = []
    index: dict[str, list[int]] = defaultdict(list)
    for i, b in enumerate(blocks):
        ordered = sorted(b.shingles, key=lambda s: (freq[s], s))
        plen = min(len(ordered), len(ordered) - int(THRESHOLD * len(ordered)) + 1)
        pre = ordered[:plen]
        prefixes.append(pre)
        for s in pre:
            index[s].append(i)
    pairs = []
    for i, b in enumerate(blocks):
        if probe is not None and b.path not in probe:
            continue
        seen = set()
        for s in prefixes[i]:
            for j in index[s]:
                if j == i or j in seen:
                    continue
                seen.add(j)
                other = blocks[j]
                if probe is not None and other.path in probe and j < i:
                    continue  # counted from the lower index
                if probe is None and j < i:
                    continue
                if not pair_allowed(b.path, other.path):
                    continue
                la, lb = len(b.shingles), len(other.shingles)
                if min(la, lb) < THRESHOLD * max(la, lb):
                    continue
                inter = len(b.shingles & other.shingles)
                jac = inter / (la + lb - inter)
                if jac >= THRESHOLD:
                    pairs.append((b, other, jac))
    return pairs


def fmt_pair(a: Block, b: Block, jac: float) -> str:
    return f"  {a.loc()} ~ {b.loc()} (jaccard {jac:.2f})"


# ---------------------------------------------------------------- modes


def changed_files(repo: str) -> set[str]:
    out = run_git(repo, "diff", "HEAD", "--name-only", "-z", "--no-renames")
    return {p for p in out.decode("utf-8", "replace").split("\0") if p and in_corpus(p)}


def key(fa: str, fb: str) -> tuple[str, str]:
    return (fa, fb) if fa <= fb else (fb, fa)


def run_default(repo: str) -> int:
    changed = changed_files(repo)
    if not changed:
        return 0
    wt_blocks = blocks_of(read_worktree(repo, worktree_files(repo)))
    head_blocks = blocks_of(read_head(repo))
    now: dict[tuple[str, str], list] = defaultdict(list)
    for a, b, jac in find_pairs(wt_blocks, changed):
        if a.path not in changed:
            a, b = b, a
        now[key(a.path, b.path)].append((a, b, jac))
    head: dict[tuple[str, str], int] = defaultdict(int)
    for a, b, _ in find_pairs(head_blocks, changed):
        head[key(a.path, b.path)] += 1
    findings = [(k, v) for k, v in sorted(now.items()) if len(v) > head[k]]
    if not findings:
        return 0
    print(f"{NAME}: a change adds a copy of an instruction block.", file=sys.stderr)
    for (fa, fb), ps in findings:
        print(
            f"{fa} <-> {fb}: {len(ps)} duplicate block pair(s) now, {head[(fa, fb)]} at HEAD",
            file=sys.stderr,
        )
        for a, b, jac in sorted(ps, key=lambda p: (p[0].start, p[1].start)):
            print(fmt_pair(a, b, jac), file=sys.stderr)
    print(
        "\nA second copy drifts from the first. Keep the text in one canonical place and\n"
        "point to it (file path, section, or the binary's --help) instead of restating it.",
        file=sys.stderr,
    )
    return 1


def run_all(repo: str) -> int:
    paths = worktree_files(repo)
    if not paths:
        raise Undetermined("--all found no corpus file to scan")
    pairs = find_pairs(blocks_of(read_worktree(repo, paths)), None)
    for a, b, jac in sorted(pairs, key=lambda p: (p[0].path, p[0].start, p[1].path)):
        print(fmt_pair(a, b, jac))
    print(f"{NAME}: {len(pairs)} duplicate block pair(s) across {len(paths)} corpus files")
    return 1 if pairs else 0


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--all", action="store_true", help="audit the whole corpus")
    args = ap.parse_args(argv)
    try:
        repo = run_git(os.getcwd(), "rev-parse", "--show-toplevel").decode().strip()
        if not args.all:
            run_git(repo, "rev-parse", "--verify", "-q", "HEAD^{commit}")
        return run_all(repo) if args.all else run_default(repo)
    except Undetermined as exc:
        print(f"{NAME}: UNDETERMINED: {exc}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
