#!/usr/bin/env python3
"""Behaviour-preservation proofs for pure-move refactors (python3 stdlib only).

Subcommands (see README.md of the refactor plugin for the full contract):

  multiset --repo R --base B --head H -- <pathspec>...
      The multiset of normalised lines over all files matching the pathspecs at
      rev B equals that at rev H (nothing was added, dropped or duplicated).
  ordered --repo R --commit C --from <path> --to <path>
      The normalised lines REMOVED from <from> by commit C, in order, equal the
      normalised lines of <to> (a file C creates), compared as whitespace-joined
      token sequences; and <from> gained nothing but use/mod lines (a line
      replaced at the same position by itself modulo normalisation, i.e. a
      visibility change, is not a gain).
  tests record  --repo R --out DIR -- <test command...>
  tests compare --repo R --baseline DIR [--allow-removed FILE] -- <test command...>
      Run a libtest-format test command in R (stdout parsed, stderr kept apart)
      and record / compare the passed and failed test-name multisets.

Exit codes for every subcommand:
  0 = proven equal
  1 = differs (a human-readable difference is printed)
  2 = undetermined: git failure, bad arguments, empty input after normalisation,
      unparsable test output. Undetermined is never reported as 0.

Normalisation N (Rust), per line, applied to both sides:
  strip whitespace; drop blank lines; drop `//!` lines; drop `use ...;` lines
  including a multi-line use up to its `;` (optional pub / pub(crate) /
  pub(super) / pub(self) / pub(in path) prefix); drop `mod name;` lines (same
  prefix); drop an attribute line directly above a dropped use/mod (e.g.
  `#[cfg(test)]`); remove ONE leading visibility qualifier from the remaining
  lines. `ordered` additionally compares re-wrap-insensitive token streams (see
  tokens()) and accepts an inline `mod NAME { BODY }` moved to NAME.rs.
  N is line-based, not a Rust parser: a `use`/`mod` line inside a multi-line
  string literal is dropped too. That is why the proofs are paired with the
  test-set comparison and the compiler.
"""

import argparse
import collections
import os
import re
import subprocess
import sys

EQUAL, DIFFERS, UNDETERMINED = 0, 1, 2

VIS = r"pub(?:\((?:crate|super|self|in\s+[\w:]+)\))?\s+"
USE_START = re.compile(r"^(?:%s)?use\s" % VIS)
MOD_DECL = re.compile(r"^(?:%s)?mod\s+(?:r#)?\w+\s*;$" % VIS)
LEADING_VIS = re.compile(r"^%s" % VIS)
# A continuation line of a multi-line `use` may only contain path-list syntax.
# Anything else ends the swallow and is KEPT (counting a line never hides it).
USE_CONT = re.compile(r"^[\w:{},*#\s;]+$")
ATTR = re.compile(r"^#\[.*\]$")


class Undetermined(Exception):
    pass


def classify(lines):
    """Return [(kept: bool, normalised text)] for each line, in order."""
    out = []
    in_use = False
    for raw in lines:
        s = raw.strip()
        if in_use:
            if USE_CONT.match(s):
                in_use = not s.endswith(";")
                out.append((False, s))
                continue
            in_use = False
        if not s or s.startswith("//!"):
            out.append((False, s))
            continue
        if USE_START.match(s):
            in_use = not s.endswith(";")
            out.append((False, s))
            continue
        if MOD_DECL.match(s):
            out.append((False, s))
            continue
        out.append((True, LEADING_VIS.sub("", s, count=1)))
    # An outer attribute line directly above a dropped use/mod statement (e.g.
    # `#[cfg(test)]` on a test-only re-import or on `mod tests;`) is part of
    # that statement and is dropped with it.
    for i in range(len(out) - 1, -1, -1):
        keep, s = out[i]
        if keep and ATTR.match(s) and i + 1 < len(out):
            nxt = out[i + 1][1]
            if not out[i + 1][0] and (USE_START.match(nxt) or MOD_DECL.match(nxt)
                                       or ATTR.match(nxt)):
                out[i] = (False, s)
    return out


def normalise(lines):
    return [t for keep, t in classify(lines) if keep]


# ---------------------------------------------------------------- git helpers


def git_bytes(repo, *args):
    try:
        p = subprocess.run(
            ["git", "-C", repo, "-c", "core.quotepath=off", *args],
            capture_output=True,
        )
    except OSError as e:
        raise Undetermined("cannot run git: %s" % e)
    if p.returncode != 0:
        raise Undetermined(
            "git %s failed (exit %d): %s"
            % (" ".join(args), p.returncode, p.stderr.decode("utf-8", "replace").strip())
        )
    return p.stdout


def git_text(repo, *args):
    data = git_bytes(repo, *args)
    try:
        return data.decode("utf-8")
    except UnicodeDecodeError:
        raise Undetermined("git %s produced non-UTF-8 output" % " ".join(args))


def resolve_commit(repo, rev):
    try:
        p = subprocess.run(
            ["git", "-C", repo, "rev-parse", "--verify", "--quiet", rev + "^{commit}"],
            capture_output=True, text=True,
        )
    except OSError as e:
        raise Undetermined("cannot run git: %s" % e)
    sha = p.stdout.strip()
    if p.returncode != 0 or not sha:
        raise Undetermined("not a commit: %r" % rev)
    return sha


def path_exists(repo, rev, path):
    try:
        p = subprocess.run(
            ["git", "-C", repo, "cat-file", "-e", "%s:%s" % (rev, path)],
            capture_output=True,
        )
    except OSError as e:
        raise Undetermined("cannot run git: %s" % e)
    return p.returncode == 0


def read_file(repo, rev, path):
    return git_text(repo, "show", "%s:%s" % (rev, path)).split("\n")


def empty_tree(repo):
    try:
        p = subprocess.run(
            ["git", "-C", repo, "hash-object", "-t", "tree", "--stdin"],
            input=b"", capture_output=True,
        )
    except OSError as e:
        raise Undetermined("cannot run git: %s" % e)
    sha = p.stdout.decode().strip()
    if p.returncode != 0 or not sha:
        raise Undetermined("cannot compute the empty tree id")
    return sha


def files_at(repo, rev, pathspecs):
    # diff-tree against the empty tree lists every file of `rev` matching the
    # pathspecs with full git pathspec semantics (globs, magic), unlike ls-tree.
    out = git_text(
        repo, "diff-tree", "-r", "-z", "--name-only", "--no-renames", "--no-commit-id",
        empty_tree(repo), rev, "--", *pathspecs,
    )
    return sorted(f for f in out.split("\0") if f)


# ---------------------------------------------------------------- multiset


def cmd_multiset(a):
    if not a.rest:
        raise Undetermined("multiset needs at least one pathspec after --")
    sides = []
    for label, rev in (("base", a.base), ("head", a.head)):
        sha = resolve_commit(a.repo, rev)
        files = files_at(a.repo, sha, a.rest)
        if not files:
            raise Undetermined("no files match %s at %s %s" % (" ".join(a.rest), label, rev))
        c = collections.Counter()
        for f in files:
            c.update(normalise(read_file(a.repo, sha, f)))
        if not c:
            raise Undetermined("%s %s is empty after normalisation" % (label, rev))
        sides.append((files, c))
    (bf, b), (hf, h) = sides
    print("base files=%d lines=%d  head files=%d lines=%d"
          % (len(bf), sum(b.values()), len(hf), sum(h.values())))
    if b == h:
        print("EQUAL: normalised line multisets match")
        return EQUAL
    only_b, only_h = b - h, h - b
    print("DIFFERS: %d line(s) only at base, %d only at head"
          % (sum(only_b.values()), sum(only_h.values())))
    for s, n in sorted(only_b.items())[:40]:
        print("  - [%d] %s" % (n, s))
    for s, n in sorted(only_h.items())[:40]:
        print("  + [%d] %s" % (n, s))
    return DIFFERS


# ---------------------------------------------------------------- ordered

HUNK = re.compile(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@")


def hunks(repo, parent, commit, path):
    """[(old_start, old_count, new_start, new_count, removed_text, added_text)]."""
    diff = git_text(
        repo, "diff", "--no-color", "--no-ext-diff", "--no-renames", "--text", "-U0",
        parent, commit, "--", ":(literal)" + path,
    )
    out = []
    cur = None
    for line in diff.split("\n"):
        m = HUNK.match(line)
        if m:
            os_, oc, ns, nc = m.groups()
            cur = [int(os_), 1 if oc is None else int(oc),
                   int(ns), 1 if nc is None else int(nc), [], []]
            out.append(cur)
        elif line.startswith("diff --git"):
            cur = None
        elif cur is not None and line.startswith("-"):
            cur[4].append(line[1:])
        elif cur is not None and line.startswith("+"):
            cur[5].append(line[1:])
    return out


def span(cls, start, count):
    # 1-based start; when count == 0 the hunk is a pure insertion/deletion point.
    return cls[start - 1:start - 1 + count] if count else []


BRACKET = re.compile(r"([()\[\]{},;])")


def tokens(lines):
    """Re-wrap-insensitive token stream of N-lines.

    rustfmt re-wrapping (typically a signature that grew past the width after a
    visibility change) moves line breaks and adds a trailing comma before the
    closing bracket of the wrapped list. Brackets/commas/semicolons are split into
    their own tokens and whitespace is insignificant. A comma that ENDS a line
    (as code, not inside a literal or comment) whose next line STARTS with the
    matching closing bracket is a re-wrap candidate (a one-line `(x,)` never is),
    and is dropped only where it cannot be semantic — see Brackets.droppable().
    Otherwise it is kept, which fails toward exit 1.
    """
    out = []
    br = Brackets()
    for i, s in enumerate(lines):
        br.feed(s)
        out.extend(BRACKET.sub(r" \1 ", s).split())
        if (s.endswith(",") and i + 1 < len(lines) and lines[i + 1][:1] in (")", "]", "}")
                and br.droppable(lines[i + 1][0])):
            out.pop()
    return out


RUST_KEYWORDS = frozenset(
    "as async await break const continue crate dyn else enum extern false fn for if impl "
    "in let loop match mod move mut pub ref return self Self static struct super trait true "
    "type unsafe use where while yield box".split()
)
OPENERS = {"(": ")", "[": "]", "{": "}"}
IDENT = re.compile(r"[A-Za-z0-9_]")
RAW_START = re.compile(r"r(#*)\"")


class Brackets:
    """Line-fed lexer tracking the open-bracket stack, skipping string / char
    literals and comments, so a trailing comma's group can be judged.

    Each frame records its kind, how many top-level commas it holds so far, and
    whether its `(` follows a non-keyword identifier or macro name (a call, a fn
    signature, a tuple-struct argument list). Anything it cannot read for sure
    (mismatched bracket, unterminated literal at the comma) makes droppable()
    answer False.
    """

    def __init__(self):
        self.stack = []  # [kind, commas, callable]
        self.broken = False
        self.in_str = False
        self.raw_hashes = None  # inside r#"..."# when not None
        self.block = 0  # nested /* */ depth
        self.last = ""  # last significant token (identifier, `name!`, or punct)
        self.code_comma_at_end = False

    def feed(self, s):
        self.code_comma_at_end = False
        k, n = 0, len(s)
        while k < n:
            c = s[k]
            if self.block:
                if s.startswith("*/", k):
                    self.block -= 1
                    k += 2
                elif s.startswith("/*", k):
                    self.block += 1
                    k += 2
                else:
                    k += 1
                continue
            if self.in_str:
                if c == "\\":
                    k += 2
                    continue
                if c == '"':
                    self.in_str = False
                    self.last = "lit"
                k += 1
                continue
            if self.raw_hashes is not None:
                end = '"' + "#" * self.raw_hashes
                if s.startswith(end, k):
                    k += len(end)
                    self.raw_hashes = None
                    self.last = "lit"
                else:
                    k += 1
                continue
            if s.startswith("//", k):
                break
            if s.startswith("/*", k):
                self.block = 1
                k += 2
                continue
            m = RAW_START.match(s, k)
            if m and (k == 0 or not IDENT.match(s[k - 1])
                      or (s[k - 1] == "b" and (k < 2 or not IDENT.match(s[k - 2])))):
                self.raw_hashes = len(m.group(1))
                k = m.end()
                continue
            if c == '"':
                self.in_str = True
                k += 1
                continue
            if c == "'":
                if k + 1 < n and s[k + 1] == "\\":
                    j = s.find("'", k + 2)
                    if j < 0:
                        self.broken = True
                        return
                    k = j + 1
                    self.last = "lit"
                    continue
                if k + 2 < n and s[k + 2] == "'":
                    k += 3
                    self.last = "lit"
                    continue
                k += 1  # lifetime: the identifier that follows is read normally
                continue
            if IDENT.match(c):
                j = k
                while j < n and IDENT.match(s[j]):
                    j += 1
                word = s[k:j]
                if j < n and s[j] == "!":
                    word += "!"
                    j += 1
                self.last = word
                k = j
                continue
            if c in OPENERS:
                callable_ = (c == "(" and re.match(r"^[A-Za-z_]\w*!?$", self.last) is not None
                             and self.last.rstrip("!") not in RUST_KEYWORDS)
                self.stack.append([c, 0, callable_])
            elif c in (")", "]", "}"):
                if not self.stack or OPENERS[self.stack[-1][0]] != c:
                    self.broken = True
                else:
                    self.stack.pop()
            elif c == ",":
                if self.stack:
                    self.stack[-1][1] += 1
                if k == n - 1:
                    self.code_comma_at_end = True
            if not c.isspace():
                self.last = c
            k += 1

    def droppable(self, close):
        if (self.broken or not self.code_comma_at_end or self.in_str
                or self.raw_hashes is not None or self.block or not self.stack):
            return False
        kind, commas, callable_ = self.stack[-1]
        if OPENERS[kind] != close:
            return False
        # `]` / `}`: a trailing comma never changes meaning. `(`: `(x,)` is a
        # 1-tuple and `(x)` is not, so only with 2+ elements or after a callee.
        return kind != "(" or commas >= 2 or callable_


def cmd_ordered(a):
    commit = resolve_commit(a.repo, a.commit)
    parent = resolve_commit(a.repo, commit + "^1")
    if not path_exists(a.repo, parent, a.frm):
        raise Undetermined("--from %s does not exist before %s" % (a.frm, a.commit))
    if not path_exists(a.repo, commit, a.to):
        raise Undetermined("--to %s does not exist at %s" % (a.to, a.commit))
    if path_exists(a.repo, parent, a.to):
        raise Undetermined("--to %s already exists before %s: not a newly created file"
                           % (a.to, a.commit))
    old_lines = read_file(a.repo, parent, a.frm)
    new_lines = read_file(a.repo, commit, a.frm) if path_exists(a.repo, commit, a.frm) else []
    old_cls, new_cls = classify(old_lines), classify(new_lines)

    removed, gained = [], []
    for os_, oc, ns, nc, rem_txt, add_txt in hunks(a.repo, parent, commit, a.frm):
        r_raw = old_lines[os_ - 1:os_ - 1 + oc] if oc else []
        a_raw = new_lines[ns - 1:ns - 1 + nc] if nc else []
        if r_raw != rem_txt or a_raw != add_txt:
            raise Undetermined("diff hunk @@ -%d,%d +%d,%d @@ does not match the file contents"
                               % (os_, oc, ns, nc))
        r = [t for k, t in span(old_cls, os_, oc) if k]
        g = [t for k, t in span(new_cls, ns, nc) if k]
        # A line replaced in place by itself modulo N (visibility / whitespace)
        # is neither removed nor gained.
        while r and g and r[0] == g[0]:
            r.pop(0)
            g.pop(0)
        while r and g and r[-1] == g[-1]:
            r.pop()
            g.pop()
        removed.extend(r)
        gained.extend(g)

    moved = normalise(read_file(a.repo, commit, a.to))
    if not moved:
        raise Undetermined("--to %s is empty after normalisation" % a.to)
    ra, rb = tokens(removed), tokens(moved)
    # Inline module -> file: `mod NAME { BODY }` removed and NAME.rs (or
    # NAME/mod.rs) created with exactly BODY is the same module.
    base = os.path.basename(a.to)
    name = os.path.basename(os.path.dirname(a.to)) if base == "mod.rs" else base[:-3]
    if base.endswith(".rs") and ra == ["mod", name, "{"] + rb + ["}"]:
        rb = ra
    if ra == rb and not gained:
        print("ORDERED-EQUAL: %d tokens moved %s -> %s; %s gained only use/mod lines"
              % (len(rb), a.frm, a.to, a.frm))
        return EQUAL
    if ra != rb:
        for i, (x, y) in enumerate(zip(ra, rb)):
            if x != y:
                lo = max(0, i - 8)
                print("DIFF at token %d:\n  removed from %s: %s\n  present in %s:   %s"
                      % (i, a.frm, " ".join(ra[lo:i + 8]), a.to, " ".join(rb[lo:i + 8])))
                break
        else:
            print("LENGTH DIFF: removed from %s=%d tokens, %s=%d tokens"
                  % (a.frm, len(ra), a.to, len(rb)))
    if gained:
        print("%s gained non-use/mod lines:" % a.frm)
        for t in gained[:40]:
            print("  + %s" % t)
    return DIFFERS


# ---------------------------------------------------------------- tests

TEST_LINE = re.compile(r"^test (.+) \.\.\.(.*)$")
IGNORED = re.compile(r"^ignored(, .*)?$")


def run_tests(repo, command):
    if not command:
        raise Undetermined("no test command given after --")
    try:
        p = subprocess.run(command, cwd=repo, capture_output=True)
    except OSError as e:
        raise Undetermined("cannot run %r: %s" % (command, e))
    out = p.stdout.decode("utf-8", "replace")
    err = p.stderr.decode("utf-8", "replace")
    if not any(l.startswith("test result:") for l in out.split("\n")):
        tail = "\n".join(err.rstrip("\n").split("\n")[-30:])
        raise Undetermined("no `test result:` line on stdout (exit %d; build failed?)\n%s"
                           % (p.returncode, tail))
    passed, failed, bad = [], [], []
    for line in out.split("\n"):
        m = TEST_LINE.match(line.rstrip("\r"))
        if not m:
            continue
        name, status = m.group(1), m.group(2)
        if not status.startswith(" "):
            bad.append(line)
            continue
        status = status[1:]
        if status == "ok":
            passed.append(name)
        elif status == "FAILED":
            failed.append(name)
        elif IGNORED.match(status):
            pass
        else:
            bad.append(line)
    if bad:
        raise Undetermined("unparsable test line(s):\n" + "\n".join("  " + b for b in bad[:20]))
    return sorted(passed), sorted(failed)


def write_list(path, names):
    with open(path, "w", encoding="utf-8") as fh:
        fh.write("".join(n + "\n" for n in names))


def read_list(path):
    try:
        with open(path, encoding="utf-8") as fh:
            return [l.rstrip("\r") for l in fh.read().split("\n") if l.strip()]
    except OSError as e:
        raise Undetermined("cannot read %s: %s" % (path, e))


def cmd_tests_record(a):
    passed, failed = run_tests(a.repo, a.rest)
    try:
        os.makedirs(a.out, exist_ok=True)
        write_list(os.path.join(a.out, "pass.txt"), passed)
        write_list(os.path.join(a.out, "fail.txt"), failed)
    except OSError as e:
        raise Undetermined("cannot write baseline to %s: %s" % (a.out, e))
    print("RECORDED: passed=%d failed=%d -> %s" % (len(passed), len(failed), a.out))
    return EQUAL


def multiset_diff(label, expect, got):
    e, g = collections.Counter(expect), collections.Counter(got)
    if e == g:
        return False
    print("%s SET DIFFERS:" % label)
    for n, k in sorted((e - g).items()):
        print("  missing: %s%s" % (n, "" if k == 1 else " (x%d)" % k))
    for n, k in sorted((g - e).items()):
        print("  new:     %s%s" % (n, "" if k == 1 else " (x%d)" % k))
    return True


def cmd_tests_compare(a):
    base_pass = read_list(os.path.join(a.baseline, "pass.txt"))
    base_fail = read_list(os.path.join(a.baseline, "fail.txt"))
    allow = set(read_list(a.allow_removed)) if a.allow_removed else set()
    passed, failed = run_tests(a.repo, a.rest)
    expect_pass = [n for n in base_pass if n not in allow]
    d1 = multiset_diff("FAILED", base_fail, failed)
    d2 = multiset_diff("PASSED", expect_pass, passed)
    print("passed=%d failed=%d baseline_passed=%d baseline_failed=%d allowed_removed=%d"
          % (len(passed), len(failed), len(base_pass), len(base_fail), len(allow)))
    if d1 or d2:
        return DIFFERS
    print("EQUAL: pass and fail sets match the baseline")
    return EQUAL


# ---------------------------------------------------------------- CLI


class Parser(argparse.ArgumentParser):
    def error(self, message):
        self.print_usage(sys.stderr)
        print("UNDETERMINED: bad arguments: %s" % message)
        sys.exit(UNDETERMINED)


def build_parser():
    p = Parser(prog="refactor_proof.py", description=__doc__.split("\n\n")[0])
    sub = p.add_subparsers(dest="cmd", required=True, parser_class=Parser)

    m = sub.add_parser("multiset", help="normalised line multiset equality across two revs")
    m.add_argument("--repo", required=True)
    m.add_argument("--base", required=True)
    m.add_argument("--head", required=True)
    m.set_defaults(func=cmd_multiset)

    o = sub.add_parser("ordered", help="one commit moved a contiguous-order block into a new file")
    o.add_argument("--repo", required=True)
    o.add_argument("--commit", required=True)
    o.add_argument("--from", dest="frm", required=True)
    o.add_argument("--to", required=True)
    o.set_defaults(func=cmd_ordered)

    t = sub.add_parser("tests", help="record / compare libtest pass+fail name sets")
    tsub = t.add_subparsers(dest="tcmd", required=True, parser_class=Parser)
    r = tsub.add_parser("record")
    r.add_argument("--repo", required=True)
    r.add_argument("--out", required=True)
    r.set_defaults(func=cmd_tests_record)
    c = tsub.add_parser("compare")
    c.add_argument("--repo", required=True)
    c.add_argument("--baseline", required=True)
    c.add_argument("--allow-removed")
    c.set_defaults(func=cmd_tests_compare)
    return p


def main(argv):
    if "--" in argv:
        i = argv.index("--")
        head, rest = argv[:i], argv[i + 1:]
    else:
        head, rest = argv, []
    a = build_parser().parse_args(head)
    if a.cmd == "ordered" and rest:
        print("UNDETERMINED: ordered takes no arguments after --")
        return UNDETERMINED
    a.rest = rest
    try:
        return a.func(a)
    except Undetermined as e:
        print("UNDETERMINED: %s" % e)
        return UNDETERMINED
    except Exception as e:  # any crash is "could not prove", never "proven"
        print("UNDETERMINED: internal error: %r" % (e,))
        return UNDETERMINED


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
