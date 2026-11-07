#!/usr/bin/env python3
"""Block a change that WEAKENS the test surface instead of the code.

Why this gate exists
--------------------
When an implementation and its tests land in the same commit, the cheapest way
to turn a red gate green is to weaken the *test*: drop an assertion, delete the
failing case, bolt `#[ignore]` on it, or wrap it in `#[should_panic]`. Every
downstream signal this repo relies on — `cargo test`'s exit code, the F->P
oracle, mutation kill-rate — reads that as success, because from their vantage
point nothing failed. `donegate.toml`'s `test-changed` check only looks at the
exit code and never inspects what happened to the tests themselves.

This scanner compares the test surface before and after a change and blocks on a
net loss. It is deliberately NOT advisory: a warning that can be ignored is the
same fail-open in slower motion.

Fail-closed contract (repo doctrine: "cannot determine" resolves to the
restricted side, never to "fine")
---------------------------------------------------------------------
  exit 0  no weakening found on the changed test surface
  exit 1  at least one UNACKNOWLEDGED weakening finding      -> block
  exit 2  the verdict could not be determined at all         -> block

Exit 2 covers: git missing or erroring, the path is not a repo, the base ref
does not resolve, a diff subprocess exits non-zero, a tracked file cannot be
read or decoded, a `#[cfg(test)]` module whose braces do not balance (an
unparsed module is an uninspected one), an out-of-line `#[cfg(test)] mod x;`
whose body file cannot be resolved on that side, or a Python test file that
`ast` cannot parse. None of these may collapse into 0: an unreadable test
surface is not a clean test surface.

Scope: every changed `.rs` file (its `#[cfg(test)]` modules -- inline bodies
and the whole file behind an out-of-line `#[cfg(test)] mod x;`, attributed to
the declaring file -- or the whole file under a `tests/` dir), and every
changed Python test file -- `test_*.py`,
`*_test.py`, or any `.py` under a `tests/` dir (backlog 911a93b8; see the
"Python test surface" section below for the Python-specific rules).

What this does NOT see, for either language: the scanner is COUNT/SHAPE-based.
A test body wrapped in dead code, an early return before the assertions, or an
assertion weakened in place (same count, weaker check) is not detected.

Acknowledging a deliberate deletion
-----------------------------------
Deleting a test that is genuinely obsolete is legitimate; silencing a test that
is red is not. The difference is not visible to a scanner, so it must be stated
by a human and left where a reviewer sees it. A commit message line

    test-weakening-justified: <path>:<kind> - <reason>

acknowledges exactly one finding. Both the path and the kind must match, and a
reason is required. A bare `test-weakening-justified:` acknowledges nothing --
a blanket pass would reintroduce the very fail-open this gate exists to stop.

Two consequences of that design, stated so they read as decisions:

* The marker is read from `base..HEAD` commit messages, from `base..MERGE_HEAD`
  while a merge is in progress (see `acknowledgements`), AND, when `--pending-msg
  FILE` is passed, from the message about to be written. Until 2026-07-30 only
  the former was read, which made the hatch unreachable exactly when it was
  needed: git runs `pre-commit` before any message exists, and on a branch's
  first commit `base..HEAD` is empty, so a staged weakening could not be
  acknowledged at all and `--no-verify` was the only way past a false positive.
  The justification is still durable in history where a reviewer meets it --
  `--pending-msg` points at the message that is being committed, not at a
  scratch file -- and it is held to the same exactness rules. This gate now runs
  from .githooks/commit-msg, where git supplies that path as $1.
* Scope is the current branch/worktree's own changes: merge-base(local `main`,
  HEAD) -- the fork point from the branch it integrates into -- vs the worktree,
  not per commit and not from `origin/main`. User ruling 2026-10-25:
  「スキャナは現在作業しているbranch,worktreeに限定した」 (GitHub #1181, backlog
  52d17300). A stale `origin/main` behind local `main` used to charge main's
  commits to every branch commit. A base that does not resolve, or a merge-base
  that cannot be computed, is Undetermined (exit 2, block); there is no fallback
  ref. Within that scope it is branch-wide, with ONE per-commit exception for a
  file that did not exist at the base. Against the base such a file has no
  coverage to lose, so it alone yields nothing; but it may already exist at HEAD,
  committed earlier on this branch, and then its tests did exist. For such a file
  the scanner ALSO compares HEAD with the worktree and blocks a skip or
  expected-failure marker gained by a test present on both (`ignore-added` /
  `should-panic-added`; `head_marker_findings`; backlog 78817f03, user ruling
  2026-11-07). That comparison deliberately does NOT judge assertion loss or
  test removal: per-commit counting of those fires on ordinary iteration (write
  test, refactor, consolidate), and a gate that fires on normal work gets
  switched off. So the gap that remains -- adding a strong new test inside a PR
  and then removing its assertions or the test itself before merge -- is still
  NOT covered by this scanner; the tdd F->P oracle (a RED observation before the
  GREEN) is the control for that, not this gate. The HEAD comparison only sees
  the step from HEAD to the worktree, so it judges each commit as it is made
  (commit-msg); a pre-push run, where the worktree equals HEAD, adds nothing.
  A marker present in the file's FIRST commit (absent at HEAD) is not judged.
* Assertion loss is counted at TWO granularities, and either one raises the
  finding. Until 2026-09-24 it was counted only NET PER FILE, and that alone was
  a hole large enough to walk a deletion through: removing two real assertions
  from one `#[test]` function and adding any two elsewhere in the SAME file
  returned `{"verdict": "clean", "findings": []}` while the deletion stood
  (reproduced at rev b47f08e3; the same deletion alone reported
  `assertion-removed`, `assertion macros 4 -> 2`). The gate exists so a weakening
  cannot pass UNACKNOWLEDGED, and that class passed unacknowledged (backlog
  `4cbfbbfc`). So a `#[test]` function that exists on BOTH sides and comes back
  with fewer assertion macros is now reported by name, whatever the file total
  did. Per-file counting is KEPT alongside it, because it still catches losses in
  the parts of the surface that are not inside a `#[test]` body (helpers,
  fixtures, `tests/` files' free functions).

  The per-function half reads a COMMENT- AND LITERAL-MASKED copy of the surface
  (`_mask_noncode`), because attributing a `#[test]` to a function requires
  knowing it is code. It also means an `assert!` that appears only inside a
  string fixture is not credited to the enclosing test. Since backlog 63e2690d
  the per-file counts (assertions / `#[test]` / `#[ignore]` / `#[should_panic]`)
  read the same masked surface, on both sides of the diff: an `#[ignore]` that
  appears only in a comment is no longer `ignore-added`, and an `assert!` in a
  doc comment can no longer offset a real assertion removal.

  What this deliberately does NOT do: pair functions across a rename. A function
  present at the base and absent at HEAD is not reported as assertion loss -- a
  rename is ordinary refactoring, and guessing which new name is "the same test"
  would fire on normal work. Its disappearance is still visible to the per-file
  `#[test]`-count check (`test-removed`). The remaining, acknowledged false
  positive is the honest one: hoisting assertions out of a test body into a
  shared helper lowers that body's count without weakening anything. That is what
  the `test-weakening-justified:` marker is for, and it is a far cheaper failure
  than the silent pass it replaces.
"""

from __future__ import annotations

import argparse
import ast
import difflib
import json
import os
import posixpath
import re
import subprocess
import sys
import warnings

# --- the weakening signals -------------------------------------------------

ASSERT_RE = re.compile(
    r"\b(?:debug_)?assert(?:_eq|_ne)?\s*!",
)
TEST_ATTR_RE = re.compile(
    r"#\s*\[\s*(?:[A-Za-z_][A-Za-z0-9_]*\s*::\s*)*test\s*\]",
)
IGNORE_ATTR_RE = re.compile(r"#\s*\[\s*ignore\b")
SHOULD_PANIC_RE = re.compile(r"#\s*\[\s*should_panic\b")

# A function declaration, capturing its name — used to attribute a `#[test]`
# attribute to the body it belongs to (see `test_fn_assertions`). A
# `macro_rules!` metavariable name (`fn $name`) is accepted too, so a `#[test]`
# inside a macro template is attributed to the template body instead of
# skipping ahead to the next real `fn` (backlog fa35fde1 / GH #1525). Such a
# function is a TEMPLATE: how many tests it expands to depends on invocations,
# which are not counted -- see `scan` for where that is judged.
FN_DECL_RE = re.compile(r"\bfn\s+(\$?[A-Za-z_][A-Za-z0-9_]*)")
# Everything Rust permits between an attribute and the `fn` it applies to:
# further attributes, visibility, and the asyncness/constness/ABI qualifiers.
# Anything else means the `#[test]` we matched is not that function's attribute.
ITEM_PREFIX_RE = re.compile(
    r"\A(?:\s|#\s*\[[^\[\]]*\]|pub(?:\s*\([^()]*\))?|async|unsafe|const"
    r'|extern(?:\s*"[^"]*")?)*\Z'
)

# Kinds, the justification marker, Undetermined and the git / file-content
# plumbing live in weakening_vcs.py (split out under the file-growth cap,
# backlog 78817f03). Imported by name so they are the same objects here. The
# gate's own directory is put on sys.path first: it is run as a script by the
# hooks and loaded by path from tests, and neither guarantees that.
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from weakening_vcs import (  # noqa: E402
    ALL_KINDS,
    JUSTIFY_RE,
    KIND_ASSERTION_REMOVED,
    KIND_IGNORE_ADDED,
    KIND_SHOULD_PANIC_ADDED,
    KIND_TEST_REMOVED,
    Undetermined,
    _PATH_ATTR_GREP,
    acknowledgements,
    blob_at,
    blob_or_none,
    changed_pairs,
    git,
    grep_path_attr_files,
    is_git_repo,
    resolve_base,
    worktree_content,
    worktree_or_none,
)


# --- test-surface extraction ----------------------------------------------


def is_tests_dir_file(path: str) -> bool:
    """Any `.rs` file with a `tests` directory component: its WHOLE content is
    test surface (see `test_surface`). Broad on purpose -- it also covers a
    `src/**/tests/` file that no out-of-line declaration claims."""
    parts = path.replace("\\", "/").split("/")
    return path.endswith(".rs") and "tests" in parts[:-1]


def is_integration_tests_file(path: str) -> bool:
    """A `.rs` file under a Cargo integration-test dir: a `tests` directory
    component that is NOT below a `src` component (`crates/c/tests/x.rs`). A
    `tests` dir inside the source tree (`src/foo/tests/mod.rs`) is a module
    path -- possibly an out-of-line `#[cfg(test)]` body -- and must be resolved
    back to its declarer, not short-circuited as an integration test."""
    if not path.endswith(".rs"):
        return False
    for part in path.replace("\\", "/").split("/")[:-1]:
        if part == "src":
            return False
        if part == "tests":
            return True
    return False


def _matching_brace(src: str, open_idx: int) -> int | None:
    """Index of the `}` matching the `{` at `open_idx`, or None if the braces do
    not balance before end of input.

    Braces inside string literals (`"..."`, raw `r#"..."#`), char literals
    (`'}'`), line comments (`// ...`) and block comments (`/* ... */`, which nest
    in Rust) are NOT counted. Counting raw `{`/`}` bytes instead lets a `}` inside
    a literal such as `assert_eq!(s, "}")` drive the depth to zero early and
    truncate the module surface, so a weakening past that point is silently
    invisible -- the fail-open this function exists to prevent.
    """
    depth = 0
    i = open_idx
    n = len(src)
    while i < n:
        c = src[i]
        # line comment -> skip to end of line
        if c == "/" and i + 1 < n and src[i + 1] == "/":
            nl = src.find("\n", i)
            i = n if nl == -1 else nl
            continue
        # block comment -> skip to the matching */ (Rust nests them)
        if c == "/" and i + 1 < n and src[i + 1] == "*":
            i += 2
            nest = 1
            while i < n and nest > 0:
                if src[i] == "/" and i + 1 < n and src[i + 1] == "*":
                    nest += 1
                    i += 2
                elif src[i] == "*" and i + 1 < n and src[i + 1] == "/":
                    nest -= 1
                    i += 2
                else:
                    i += 1
            continue
        # raw string r"...", r#"..."#, r##"..."## (no escapes inside)
        if c == "r" and i + 1 < n and src[i + 1] in '#"':
            j = i + 1
            hashes = 0
            while j < n and src[j] == "#":
                hashes += 1
                j += 1
            if j < n and src[j] == '"':
                close = '"' + "#" * hashes
                end = src.find(close, j + 1)
                if end == -1:
                    return None  # unterminated raw string: we cannot claim a read
                i = end + len(close)
                continue
            # `r` not opening a raw string after all: fall through as a plain byte
        # normal string "..." with backslash escapes
        if c == '"':
            i += 1
            while i < n:
                if src[i] == "\\":
                    i += 2
                    continue
                if src[i] == '"':
                    i += 1
                    break
                i += 1
            continue
        # char literal 'x' / '\n' / '{' vs a lifetime 'a (which has no closing ')
        if c == "'":
            if i + 1 < n and src[i + 1] == "\\":
                # Escaped char literal: '\n' '\t' '\\' '\'' '\x41' '\u{7f}'. The
                # byte right after the backslash is escaped CONTENT (which may
                # itself be '\' as in '\\', or a quote as in '\''), never the
                # closing quote and never a fresh escape lead -- so skip it, then
                # scan to the closing quote. No char-escape form contains an
                # unescaped ' before its real closing quote, so this is exact.
                #
                # The old scan started at i+2 and re-read that content byte AS an
                # escape lead: for '\\' it saw the second backslash, did j += 2,
                # and jumped PAST the closing quote, then ran on to the NEXT ' in
                # the file -- swallowing every { and } in between. store.rs (two
                # `.contains('\\')` assertions) hit exactly this, and the run-on
                # is precisely the surface truncation this function exists to
                # prevent (a weakening hidden past the first '\\' would vanish).
                j = i + 3
                while j < n and src[j] != "'":
                    j += 1
                if j < n:
                    i = j + 1
                    continue
                # unterminated: treat the quote as ordinary punctuation
            elif i + 2 < n and src[i + 2] == "'":
                i += 3
                continue
            # a lifetime/label like 'a: consume just the quote and move on
            i += 1
            continue
        if c == "{":
            depth += 1
        elif c == "}":
            depth -= 1
            if depth == 0:
                return i
        i += 1
    return None


def _mask_noncode(src: str, path: str) -> str:
    """`src` with every comment and string/char literal blanked to spaces.

    Length and newlines are preserved, so every index into the result is also an
    index into `src` and the two can be sliced interchangeably.

    This exists because `#[test]` is a string of nine characters that occurs
    freely in this repo's PROSE: doc comments explaining the gate, and Rust
    fixtures embedded in string literals by the gate crates' own tests. Measured
    2026-09-24 at rev b47f08e3 over all 636 `.rs` files in the repo: matching the
    attribute regex against raw source misattributes it in **18** files (e.g.
    `crates/tdd/src/config.rs`, `crates/overwatch/src/store.rs`,
    `crates/harness-core/tests/change_attribution.rs`), which is precisely the
    known false-positive class in backlog `10663`/`10133`. Masking first drops
    that to **0 of 636**, with 5113 test functions attributed — re-measured the
    same day with the same scan. That measurement is what makes the
    `Undetermined` above affordable: without it, 18 files would have blocked
    every commit that touched them with exit 2, and a gate that fires on normal
    work gets switched off.

    An unterminated comment or literal raises: we cannot claim to have read a
    file we cannot tokenise, and this scanner's contract is that an uninspected
    surface is never a clean one.
    """
    out = list(src)
    n = len(src)

    def blank(a: int, b: int) -> None:
        for k in range(a, b):
            if out[k] != "\n":
                out[k] = " "

    i = 0
    while i < n:
        c = src[i]
        if c == "/" and i + 1 < n and src[i + 1] == "/":
            nl = src.find("\n", i)
            end = n if nl == -1 else nl
            blank(i, end)
            i = end
            continue
        if c == "/" and i + 1 < n and src[i + 1] == "*":
            j = i + 2
            nest = 1
            while j < n and nest > 0:
                if src[j] == "/" and j + 1 < n and src[j + 1] == "*":
                    nest += 1
                    j += 2
                elif src[j] == "*" and j + 1 < n and src[j + 1] == "/":
                    nest -= 1
                    j += 2
                else:
                    j += 1
            if nest > 0:
                raise Undetermined(f"{path}: unterminated block comment")
            blank(i, j)
            i = j
            continue
        if c == "r" and i + 1 < n and src[i + 1] in '#"':
            j = i + 1
            hashes = 0
            while j < n and src[j] == "#":
                hashes += 1
                j += 1
            if j < n and src[j] == '"':
                close = '"' + "#" * hashes
                end = src.find(close, j + 1)
                if end == -1:
                    raise Undetermined(f"{path}: unterminated raw string literal")
                blank(i, end + len(close))
                i = end + len(close)
                continue
            # `r` not opening a raw string after all: an ordinary identifier byte.
        if c == '"':
            j = i + 1
            closed = False
            while j < n:
                if src[j] == "\\":
                    j += 2
                    continue
                if src[j] == '"':
                    j += 1
                    closed = True
                    break
                j += 1
            if not closed:
                raise Undetermined(f"{path}: unterminated string literal")
            blank(i, j)
            i = j
            continue
        if c == "'":
            # Escaped char literal / plain char literal / lifetime — the same
            # three cases, resolved the same way, as in `_matching_brace`.
            if i + 1 < n and src[i + 1] == "\\":
                j = i + 3
                while j < n and src[j] != "'":
                    j += 1
                if j < n:
                    blank(i, j + 1)
                    i = j + 1
                    continue
                i += 1
                continue
            if i + 2 < n and src[i + 2] == "'":
                blank(i, i + 3)
                i += 3
                continue
            i += 1
            continue
        i += 1
    return "".join(out)


# --- out-of-line modules (`#[cfg(test)] mod tests;`) ----------------------
#
# Moving an inline `#[cfg(test)] mod tests { ... }` into its own file is an
# ordinary, correct refactor. Until 2026-11-03 the declaration form
# `#[cfg(test)] mod tests;` had no body for `cfg_test_modules` to find, so every
# such file went Undetermined (exit 2) and the refactor could not be committed.
#
# Now the body FILE is resolved with Rust's module path rules and its whole
# contents are test surface, read from the same side as the declaring file (the
# merge-base blob for the base, the worktree for the change). The body is
# appended to the DECLARING file's surface, so findings stay keyed by the
# declaring path on both sides: a verbatim inline -> out-of-line move compares
# equal, while an assertion or `#[test]` lost in the move is still reported (it
# is attributed to the declaring file, e.g. `src/foo.rs`, not `src/foo/tests.rs`).
# The body file is NOT also scanned under its own path (see `test_body_root`),
# so it is neither double-counted nor read as production code with an empty
# surface. Further `mod x;` declarations inside a body file are followed the
# same way (everything under a `#[cfg(test)]` module is test-only).
#
# Every case this cannot resolve is Undetermined, never clean: no body file on
# that side, both `name.rs` and `name/mod.rs` present (rustc E0761), a
# `#[path]` that is not a plain escape-free string literal (`cfg_attr(.., path
# = ..)` included), an absolute or repo-escaping `#[path]`, an out-of-line
# declaration nested inside a block that is not a plain inline `mod NAME {`
# (a fn body, or an inline module carrying a path-shaped attribute), a
# `#[path]` on a declaration nested inside an inline module, or one inside a
# `#[path]`-loaded body file (whose own child-path rules this does not model).
# A declaration inside plain inline modules (`mod a { #[cfg(test)] mod b; }`
# in `src/lib.rs` -> `src/a/b.rs`) is resolved, and one inside an inline
# `#[cfg(test)] mod x { mod y; }` counts as test-only too.
#
# What this does NOT see: a body file whose declarer is neither a standard
# parent-module file (`<dir>.rs`, `<dir>/mod.rs`, `<dir>/lib.rs`,
# `<dir>/main.rs`) of ANY ancestor directory of the body file nor a `#[path]`
# declarer findable by `git grep`. When such a body file changes WITHOUT its
# declarer it is classified like any other non-test `.rs` file (its own
# `#[cfg(test)]` modules only).

CFG_TEST_RE = re.compile(r"#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]")
MOD_DECL_RE = re.compile(r"\bmod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;")
VIS_TAIL_RE = re.compile(r"(?<![A-Za-z0-9_])pub(?:\s*\([^()]*\))?\s*\Z")
PATH_ATTR_MASKED_RE = re.compile(r"#\s*\[\s*path\s*=\s*\]")
PATH_ATTR_RAW_RE = re.compile(r'#\s*\[\s*path\s*=\s*"([^"\\]*)"\s*\]')
PATH_WORD_RE = re.compile(r"\bpath\b")
# crate-root and `mod.rs` files resolve `mod x;` next to themselves.
_MOD_RS_NAMES = ("mod.rs", "lib.rs", "main.rs")


class Side:
    """One side of the diff: `read(path)` is the file's content, or None when the
    file genuinely does not exist on that side (a read failure raises)."""

    def __init__(self, label: str, reader, grep_path_attr_files):
        self.label = label
        self._reader = reader
        self._grep = grep_path_attr_files
        self._cache: dict = {}
        self._path_files = None

    def read(self, path: str):
        if path not in self._cache:
            self._cache[path] = self._reader(path)
        return self._cache[path]

    def path_attr_files(self) -> list:
        """`.rs` files outside integration-test dirs on this side containing a
        `#[path` attribute."""
        if self._path_files is None:
            self._path_files = [p for p in self._grep() if not is_integration_tests_file(p)]
        return self._path_files


def _is_mod_rs_like(path: str) -> bool:
    return posixpath.basename(path) in _MOD_RS_NAMES


def _attr_spans_before(code: str, idx: int) -> list:
    """`(start, end)` of the outer attributes directly preceding the item whose
    keyword starts at `idx` (an optional `pub`/`pub(..)` is skipped first)."""
    j = idx
    vm = VIS_TAIL_RE.search(code, max(0, j - 80), j)
    if vm:
        j = vm.start()
    spans = []
    while True:
        k = j
        while k > 0 and code[k - 1].isspace():
            k -= 1
        if k == 0 or code[k - 1] != "]":
            break
        o = code.rfind("[", 0, k - 1)
        if o == -1 or "]" in code[o + 1 : k - 1]:
            break
        h = o - 1
        while h >= 0 and code[h].isspace():
            h -= 1
        if h < 0 or code[h] != "#":
            break
        spans.append((h, k))
        j = h
    return spans


def _module_decls(code: str, path: str) -> list:
    """Every out-of-line `mod NAME;` in the MASKED source `code`, as dicts with
    `name`, `gated` (a `#[cfg(test)]` among its attributes), `attrs` (spans),
    `depth` (brace depth: 0 = file top level) and `path_attr` (str or None)."""
    out = []
    for m in MOD_DECL_RE.finditer(code):
        attrs = _attr_spans_before(code, m.start())
        out.append({
            "name": m.group(1),
            "start": m.start(),
            "attrs": attrs,
            "gated": any(CFG_TEST_RE.fullmatch(code[a:b]) for a, b in attrs),
            "depth": code.count("{", 0, m.start()) - code.count("}", 0, m.start()),
            "enclosing": _enclosing_blocks(code, m.start()),
        })
    return out


INLINE_MOD_TAIL_RE = re.compile(r"(?<![A-Za-z0-9_])mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*\Z")


def _enclosing_blocks(code: str, idx: int) -> list:
    """The `{` blocks of the MASKED source `code` that enclose `idx`, outermost
    first, as dicts: `name` (the inline module name when the block is the body
    of `mod NAME {`, else None), `gated` (that module carries `#[cfg(test)]`)
    and `path_attr` (that module carries a `path`-shaped attribute).

    This never raises: whether an enclosing block can be resolved is decided
    by the caller that needs it (`_decl_candidates`), so an unrelated
    unresolvable block does not turn every scan of the file undetermined."""
    stack = []
    for k in range(idx):
        c = code[k]
        if c == "{":
            stack.append(k)
        elif c == "}" and stack:
            stack.pop()
    out = []
    for o in stack:
        mm = INLINE_MOD_TAIL_RE.search(code, max(0, o - 200), o)
        if mm is None:
            out.append({"name": None, "gated": False, "path_attr": False})
            continue
        attrs = _attr_spans_before(code, mm.start())
        out.append({
            "name": mm.group(1),
            "gated": any(CFG_TEST_RE.fullmatch(code[a:b]) for a, b in attrs),
            "path_attr": any(PATH_WORD_RE.search(code[a:b]) for a, b in attrs),
        })
    return out


def _decl_test_gated(decl: dict) -> bool:
    """`decl` is test-only code: its own attributes or an enclosing inline
    module carry `#[cfg(test)]`."""
    return decl["gated"] or any(e["gated"] for e in decl["enclosing"])


def _decl_path_attr(src: str, code: str, path: str, decl: dict):
    """The `#[path = "..."]` value of `decl`, None when it has none. Anything
    path-shaped that is not a plain escape-free literal is Undetermined."""
    found = None
    for a, b in decl["attrs"]:
        if not PATH_WORD_RE.search(code[a:b]):
            continue
        rm = PATH_ATTR_RAW_RE.fullmatch(src[a:b])
        if not PATH_ATTR_MASKED_RE.fullmatch(code[a:b]) or rm is None or found is not None:
            raise Undetermined(
                f"{path}: cannot resolve `mod {decl['name']};` -- non-trivial path "
                f"attribute {src[a:b]!r}"
            )
        found = rm.group(1)
    return found


def _decl_candidates(src: str, code: str, path: str, decl: dict, via_path_attr: bool) -> list:
    """Repo-relative candidate body files for an out-of-line `decl` in `path`."""
    inline = []
    for e in decl["enclosing"]:
        if e["name"] is None or e["path_attr"]:
            raise Undetermined(
                f"{path}: out-of-line `mod {decl['name']};` inside a block that is not "
                "a plain inline `mod NAME {` -- its body path is not resolved"
            )
        inline.append(e["name"])
    d = posixpath.dirname(path)
    attr = _decl_path_attr(src, code, path, decl)
    if attr is not None and inline:
        raise Undetermined(
            f"{path}: #[path] on `mod {decl['name']};` inside an inline module -- "
            "its body path is not resolved"
        )
    if attr is not None:
        joined = posixpath.normpath(posixpath.join(d, attr))
        if posixpath.isabs(attr) or joined == ".." or joined.startswith("../"):
            raise Undetermined(f"{path}: #[path = {attr!r}] leaves the repository")
        return [joined]
    if via_path_attr:
        raise Undetermined(
            f"{path}: `mod {decl['name']};` inside a #[path]-loaded body file -- "
            "its body path is not resolved"
        )
    if _is_mod_rs_like(path):
        home = d
    else:
        home = path[: -len(".rs")]
    # `mod a { mod b; }` in `x/lib.rs` loads `x/a/b.rs` (or `x/a/b/mod.rs`).
    for name in inline:
        home = posixpath.join(home, name)
    return [
        posixpath.join(home, decl["name"] + ".rs"),
        posixpath.join(home, decl["name"], "mod.rs"),
    ]


def _resolve_body(side: Side, src: str, code: str, path: str, decl: dict, via: bool):
    """`(body_path, content, loaded_via_path_attr)` -- exactly one candidate must
    exist on `side`, else Undetermined."""
    cands = _decl_candidates(src, code, path, decl, via)
    hits = [(c, side.read(c)) for c in cands]
    hits = [(c, s) for c, s in hits if s is not None]
    if len(hits) != 1:
        why = "no body file" if not hits else "more than one body file"
        raise Undetermined(
            "{}: #[cfg(test)] `mod {};` declared but {} found at {} (tried {})".format(
                path, decl["name"], why, side.label, ", ".join(cands)
            )
        )
    return hits[0][0], hits[0][1], _decl_path_attr(src, code, path, decl) is not None


def _body_surface(side: Side, body_path: str, content: str, via: bool, seen: set) -> list:
    """Chunks for one out-of-line test body file: its whole content plus, recursively,
    every out-of-line module it declares."""
    if body_path in seen:
        raise Undetermined(f"{body_path}: module declaration cycle")
    seen.add(body_path)
    chunks = [content]
    code = _mask_noncode(content, body_path)
    for decl in _module_decls(code, body_path):
        bp, bs, bvia = _resolve_body(side, content, code, body_path, decl, via)
        chunks.extend(_body_surface(side, bp, bs, bvia, seen))
    return chunks


def test_body_root(side: Side, f: str, _seen=None):
    """The declaring (non-test) file whose `#[cfg(test)] mod x;` makes `f` an
    out-of-line test body on `side`, or None when `f` is not one.

    Candidates are the standard parent-module files of every ancestor directory
    (a declaration inside inline `mod a { .. }` blocks lives higher up) and
    every `#[path]` declarer on that side; a same-named declaration that cannot
    be resolved raises Undetermined. A `mod x;` that is not itself
    `#[cfg(test)]` still counts when an enclosing inline module is
    `#[cfg(test)]`, or when its declarer is in turn a test body (nested
    `tests/helpers.rs`).

    Only integration-test files are excluded up front: a `tests` dir inside the
    source tree (`src/foo/tests/mod.rs`, `src/foo/tests/helpers.rs`) is a module
    path and is resolved like any other. Excluding it with the broad
    `is_tests_dir_file` used to leave a newly added `src/foo/tests/mod.rs` next
    to an existing `src/foo/tests.rs` scanned on its own, so the declarer was
    never re-read and the E0761 ambiguity reported clean."""
    if not _is_rs(f) or is_integration_tests_file(f):
        return None
    seen = _seen if _seen is not None else set()
    if f in seen:
        return None
    seen.add(f)
    loc = posixpath.dirname(f) if posixpath.basename(f) == "mod.rs" else f[: -len(".rs")]
    # The declarer may sit at any ancestor level: `mod a { mod tests; }` in
    # `src/lib.rs` declares `src/a/tests.rs`. Walk every ancestor directory and
    # let `_decl_candidates` (which models the inline nesting) decide.
    cands = []
    d = posixpath.dirname(loc)
    while True:
        if d:
            cands.append(d + ".rs")
        cands += [posixpath.join(d, n) for n in _MOD_RS_NAMES]
        if not d:
            break
        d = posixpath.dirname(d)
    cands += side.path_attr_files()
    for c in dict.fromkeys(cands):
        if c == f or is_integration_tests_file(c):
            continue
        src = side.read(c)
        if src is None:
            continue
        code = _mask_noncode(src, c)
        for decl in _module_decls(code, c):
            attr = _decl_path_attr(src, code, c, decl)
            if attr is None and decl["name"] != posixpath.basename(loc):
                continue
            # A same-named declaration we cannot resolve might be `f`'s
            # declarer: Undetermined (raised here), never "not a test body".
            if f not in _decl_candidates(src, code, c, decl, False):
                continue
            if _decl_test_gated(decl):
                return c
            root = test_body_root(side, c, seen)
            if root is not None:
                return root
    return None


# First words that open an ITEM or a `let` statement. Such a head may contain
# depth-0 commas (`fn f<A, B>()`, `-> Map<K, V>`, `where T: A, U: B`,
# `impl<A, B> X<A, B>`), so it ends only at its block `{` or at `;`.
# Only STRICT keywords are listed: none of them can be a field, variant or
# binding name, so a head starting with one is an item.
_ITEM_HEAD_WORDS = frozenset({
    "fn", "impl", "trait", "mod", "struct", "enum", "type", "use",
    "static", "const", "let", "extern", "unsafe", "async",
})
# WEAK keywords (`union`, `default`, `auto`, `macro_rules`) are also legal
# identifiers -- `struct S { #[cfg(test)] default: u8 }` is a field. They open
# an item only when followed by that item's syntax: `union NAME`,
# `default fn|impl|unsafe|const|type|async|extern`, `auto trait`,
# `macro_rules!`. Otherwise the head is classified like any other non-item.
_WEAK_ITEM_HEAD_RE = re.compile(
    r"(?:union\s+[A-Za-z_][A-Za-z0-9_]*"
    r"|default\s+(?:fn|impl|unsafe|const|type|async|extern)\b"
    r"|auto\s+trait\b"
    r"|macro_rules\s*!)"
)
_HEAD_WORD_RE = re.compile(r"\s*(?:pub\s*(?:\([^()]*\))?\s*)?([A-Za-z_][A-Za-z0-9_]*)")
_OUTER_ATTR_RE = re.compile(r"\s*#\s*\[")


def _angle_depth(seg: str) -> int:
    """`<` minus `>` in masked `seg`, not counting the arrows `->`/`=>` nor the
    two-character operators `<=` `>=` `<<` `>>`. Positive means an unclosed `<`
    (an open generic list -- or a `<` comparison: the caller cannot tell)."""
    for op in ("->", "=>", "<=", ">=", "<<", ">>"):
        seg = seg.replace(op, "  ")
    return seg.count("<") - seg.count(">")


def _annotated_block(code: str, path: str, attr_end: int):
    """Half-open `(lo, hi)` span of the test surface contributed by the item
    that the outer attribute ending at `attr_end` annotates, or None when that
    item contains no block at all.

    `code` is MASKED (no comments/literals), so every bracket is real. Further
    outer attributes are skipped first. Then:

    * an ITEM or `let` head (a strict keyword in `_ITEM_HEAD_WORDS`, or a
      weak keyword followed by its item syntax per `_WEAK_ITEM_HEAD_RE`, after
      an optional `pub(..)`) ends at its first `{` (its block: the span is
      that `{ .. }`) or `;` at `(`/`[` depth 0. Commas do not end it: they
      occur in generics, return types and where clauses (`fn f<A, B>() where
      T: A, U: B { .. }`). `#[cfg(test)] use x;` therefore captures nothing,
      never the next item's block. A `}` reached first closes an ENCLOSING
      block, so the head was misread (it is not an item): Undetermined, never
      the next item's block.
    * any other head (a struct field, enum variant, match arm, expression
      statement or list element) ends at the first depth-0 `{` (its block),
      or at `;`, `}` or a closing `)`/`]`, or at a `,` -- unless an unclosed
      `<` precedes that comma, in which case the comma may belong to a generic
      list or to a comparison and the item's extent cannot be decided:
      Undetermined, never an empty surface.

    An item that ends without a depth-0 block but has braces nested inside
    `(..)`/`[..]` (`#[cfg(test)] LOCK.with(|p| { .. });`, `const X: T =
    f(|| { .. });`) contributes its whole text up to the terminator, so a block
    inside a call is never dropped. Unbalanced braces or running out of input
    is Undetermined."""
    n = len(code)
    i = attr_end
    while True:
        am = _OUTER_ATTR_RE.match(code, i)
        if am is None:
            break
        o = am.end() - 1
        depth = 0
        k = o
        while k < n:
            if code[k] == "[":
                depth += 1
            elif code[k] == "]":
                depth -= 1
                if depth == 0:
                    break
            k += 1
        if k >= n:
            raise Undetermined(f"{path}: unterminated attribute after #[cfg(test)]")
        i = k + 1
    hm = _HEAD_WORD_RE.match(code, i)
    item_head = hm is not None and (
        hm.group(1) in _ITEM_HEAD_WORDS
        or _WEAK_ITEM_HEAD_RE.match(code, hm.start(1)) is not None
    )
    start = i
    depth = 0
    nested_brace = False

    def done(end: int):
        return (start, end) if nested_brace else None

    while i < n:
        c = code[i]
        if c in "([":
            depth += 1
        elif c in ")]":
            depth -= 1
            if depth < 0:
                return done(i)  # attribute on the last element of a list/call
        elif depth > 0:
            if c == "{":
                nested_brace = True
        else:
            if c == "{":
                end = _matching_brace(code, i)
                if end is None:
                    raise Undetermined(f"{path}: unbalanced braces in #[cfg(test)] item")
                return (i, end + 1)
            if c == ";":
                return done(i + 1)
            if item_head and c == "}":
                raise Undetermined(
                    f"{path}: #[cfg(test)] item {code[start:i].strip()[:60]!r} reaches "
                    "the end of its enclosing block without a `{` or `;`"
                )
            if not item_head:
                if c == "}":
                    return done(i)
                if c == ",":
                    if _angle_depth(code[start:i]) > 0:
                        raise Undetermined(
                            f"{path}: cannot tell where the #[cfg(test)] item "
                            f"{code[start:i].strip()[:60]!r} ends (comma after an "
                            "unclosed `<`)"
                        )
                    return done(i)
        i += 1
    raise Undetermined(f"{path}: #[cfg(test)] annotates no item before end of file")


def cfg_test_modules(
    src: str, path: str, side: Side | None = None, out_of_line_only: bool = False
) -> str:
    """Concatenate every `#[cfg(test)]` item body found in `src`.

    Attributes are matched in CODE only: `src` is masked first (see
    `_mask_noncode`), so `#[cfg(test)]` text inside a comment or a string/char/
    raw-string literal is not an attribute. An unterminated comment/literal is
    Undetermined.

    Brace matching skips string/char literals and comments (see
    `_matching_brace`); if a module's braces still do not balance we cannot claim
    to have inspected it, so the whole run goes undetermined rather than
    reporting on a partial read.

    Every out-of-line `mod NAME;` that is test-only -- its own attributes carry
    `#[cfg(test)]`, or it sits inside an inline `#[cfg(test)] mod x { .. }` --
    contributes its resolved body FILE's whole content, read from `side` (see
    the out-of-line section above; inline `mod a { mod b; }` nesting is
    followed); unresolvable is Undetermined. Any other `#[cfg(test)]`
    contributes the block of the item it annotates (`_annotated_block`; item
    heads with generic or where-clause commas keep their block, and an item
    whose braces sit inside a call contributes its whole text); an item with
    no block anywhere (`#[cfg(test)] use x;`, a plain field, a match arm
    ending in `,`) contributes nothing, and an attribute with no item after
    it, or one whose item extent cannot be decided, is Undetermined.
    With `out_of_line_only` only the out-of-line bodies are returned (used when
    the file's own content is already counted whole; see `test_surface`).
    """
    code = _mask_noncode(src, path)
    chunks = []
    decl_attrs: set = set()
    if MOD_DECL_RE.search(code):
        for decl in _module_decls(code, path):
            if not _decl_test_gated(decl):
                continue
            for a, b in decl["attrs"]:
                if CFG_TEST_RE.fullmatch(code[a:b]):
                    decl_attrs.add(a)
            if side is None:
                raise Undetermined(f"{path}: `mod {decl['name']};` with no side to read")
            bp, bs, via = _resolve_body(side, src, code, path, decl, False)
            chunks.extend(_body_surface(side, bp, bs, via, set()))
    if out_of_line_only:
        return "\n".join(chunks)
    for m in CFG_TEST_RE.finditer(code):
        if m.start() in decl_attrs:
            continue
        span = _annotated_block(code, path, m.end())
        if span is None:
            continue
        chunks.append(src[span[0] : span[1]])
    return "\n".join(chunks)


def test_surface(src: str, path: str, side: Side | None = None) -> str:
    if is_integration_tests_file(path):
        return src
    if is_tests_dir_file(path):
        # A `src/**/tests/` file that `scan` did not claim as an out-of-line
        # body of some declarer: keep the whole-file classification (never
        # downgrade it to production code), and ALSO follow its own
        # `#[cfg(test)] mod x;` bodies -- its inline modules are already inside
        # `src`, so only the out-of-line ones are added.
        extra = cfg_test_modules(src, path, side, out_of_line_only=True)
        return src + ("\n" + extra if extra else "")
    return cfg_test_modules(src, path, side)


def test_fn_assertions(surface: str, path: str) -> dict:
    """Assertion-macro count per `#[test]` function body, keyed by function name.

    This is the per-function half of the assertion check (see the module
    docstring): it makes a deletion inside one test visible even when another
    test in the same file gained assertions and the file total did not move.

    Attribution is exact rather than best-effort. Between a `#[test]` and its
    `fn` only attributes, visibility, `async`/`unsafe`/`const` and `extern "C"`
    may appear; anything else means this `#[test]` is not the attribute of the
    function we found (the usual cause is the literal text `#[test]` inside a
    string or a doc comment -- which this repo's own gate tests contain). We
    cannot attribute it, so we do not guess: that raises `Undetermined` and the
    whole run blocks with exit 2, the same answer an unbalanced `#[cfg(test)]`
    module already gets. Silently skipping the occurrence instead would leave
    exactly one un-inspected test function reported as clean, which is the
    fail-open this function was added to close.

    Names are keys, so two same-named test functions in different `#[cfg(test)]`
    modules of one file SUM. A decrease in the sum is still a real decrease, and
    the alternative (dropping duplicates) would hide one of them.

    A `#[test] fn $name` inside a `macro_rules!` template is keyed by its
    metavariable (`$name`); `counts` reports how many such templates exist so
    `scan` can refuse to judge a change against a base that has the file.
    """
    out: dict = {}
    code = _mask_noncode(surface, path)
    for m in TEST_ATTR_RE.finditer(code):
        fm = FN_DECL_RE.search(code, m.end())
        if fm is None:
            raise Undetermined(f"{path}: a #[test] attribute with no `fn` after it")
        between = code[m.end() : fm.start()]
        if not ITEM_PREFIX_RE.match(between):
            raise Undetermined(
                "{}: cannot attribute a #[test] to a function body "
                "(unexpected text before `fn`: {!r})".format(path, between.strip()[:80])
            )
        brace = code.find("{", fm.end())
        if brace == -1:
            raise Undetermined(f"{path}: #[test] fn {fm.group(1)} has no body")
        end = _matching_brace(code, brace)
        if end is None:
            raise Undetermined(f"{path}: unbalanced braces in #[test] fn {fm.group(1)}")
        body = code[brace : end + 1]
        out[fm.group(1)] = out.get(fm.group(1), 0) + len(ASSERT_RE.findall(body))
    return out


def counts(surface: str, path: str = "") -> dict:
    # Counted over the comment- and literal-MASKED surface, symmetrically for
    # both sides of the diff (backlog 63e2690d): an `#[ignore]` / `assert!` /
    # `#[test]` mentioned in a comment or string is prose, not code, and must
    # neither be reported as added nor offset a real removal.
    code = _mask_noncode(surface, path)
    fn_assertions = test_fn_assertions(surface, path)
    return {
        "assertions": len(ASSERT_RE.findall(code)),
        "tests": len(TEST_ATTR_RE.findall(code)),
        "ignores": len(IGNORE_ATTR_RE.findall(code)),
        "should_panics": len(SHOULD_PANIC_RE.findall(code)),
        "fn_assertions": fn_assertions,
        # `#[test]` functions inside a `macro_rules!` template (`fn $name`).
        "template_tests": sum(1 for n in fn_assertions if n.startswith("$")),
    }


# --- Python test surface (backlog 911a93b8) -------------------------------
#
# Until 2026-10-14 only `.rs` was scanned, so gutting `scripts/test_*.py` (the
# repo's gate tests) returned `{"verdict": "clean", "findings": []}` and the
# `test-weakening-justified:` marker could not even name a `.py` finding.
#
# Python is read with `ast`, not regexes: the module is parsed by the running
# interpreter, so comments and string literals are never mistaken for code. A
# file that does not parse is UNDETERMINED (exit 2) -- an unparsed test file is
# an uninspected one, the same answer an unbalanced `#[cfg(test)]` gets.
#
# The four Rust kinds are reused so the acknowledgment vocabulary stays one set:
#   assertion-removed   `assert` statements + `assert*()` / `pytest.raises()` calls
#                       (file total, AND per surviving test function)
#   test-removed        number of `test*` functions (module level / in classes)
#   ignore-added        a SURVIVING test gained a skip: `unittest.skip/skipIf/
#                       skipUnless`, `pytest.mark.skip/skipif`, this repo's
#                       `open_defect(...)` wrapper, `self.skipTest()`,
#                       `pytest.skip()`, `raise SkipTest`
#   should-panic-added  a SURVIVING test gained `expectedFailure` /
#                       `pytest.mark.xfail` / `pytest.xfail()` -- like
#                       `#[should_panic]`, it turns a failing test into a pass
#
# Skips are compared PER SURVIVING TEST, not per file, deliberately differently
# from Rust's file-level `#[ignore]` count: this repo's "no test, no ticket"
# workflow adds NEW red repro tests wrapped in `open_defect(...)` to existing
# files every day, and a new skipped test hides no coverage that existed. A skip
# that lands on a test which existed at the base does. A skip placed on a class,
# in its `setUp`/`setUpClass`, or at module level (`pytestmark`, a module-level
# `pytest.skip`/`raise SkipTest`) counts against every test it covers. Removing
# a skip is never reported -- that is strengthening.
#
# Skip markers are recognised through the module's import table as well as by
# last name (`from unittest import skip as nope`; `from pytest import mark as m`
# then `@m.skip`). Also counted: `__test__ = False` at module level or as a
# class attribute, skips raised/called in `setUpModule`, `pytest.importorskip`
# (module level or in a test body), and module-level monkey-skips such as
# `T.test_a = unittest.skip('x')(T.test_a)` or `setattr(T, "test_a", ...)`.
#
# LIMITATION, stated so the gate claims no more than it does: like the `.rs`
# scanner this is COUNT/SHAPE-based. It does not detect a test body wrapped in
# dead code (`if False:`), an early `return` before the assertions, an assertion
# weakened in place (`assertEqual(x, 1)` -> `assertTrue(True)`), or a skip
# reached through indirection it cannot name statically (a decorator built at
# runtime, a skip applied from another module).

PY_SKIP_DECORATORS = {"skip", "skipIf", "skipUnless", "skipif", "open_defect"}
PY_XFAIL_DECORATORS = {"expectedFailure", "xfail"}
PY_SKIP_CALLS = {"skipTest", "skip"}
PY_XFAIL_CALLS = {"xfail"}


def is_py_test_file(path) -> bool:
    if not path or not path.endswith(".py"):
        return False
    parts = path.replace("\\", "/").split("/")
    base = parts[-1]
    return base.startswith("test_") or base.endswith("_test.py") or "tests" in parts[:-1]


def _dotted(node) -> str:
    """`a.b.c` for a Name/Attribute chain (a Call is unwrapped to its func)."""
    if isinstance(node, ast.Call):
        node = node.func
    names = []
    while isinstance(node, ast.Attribute):
        names.append(node.attr)
        node = node.value
    if isinstance(node, ast.Name):
        names.append(node.id)
    return ".".join(reversed(names))


def _py_import_aliases(tree) -> dict:
    """Local name -> fully dotted origin for every import in the module.

    `from unittest import skip as nope` maps `nope` to `unittest.skip`;
    `import pytest as pt` maps `pt` to `pytest`; `from pytest import mark as m`
    maps `m` to `pytest.mark`. Imports anywhere in the file are collected, so an
    alias cannot hide a skip marker behind a name the last-name match misses.
    """
    out = {}
    for n in ast.walk(tree):
        if isinstance(n, ast.Import):
            for a in n.names:
                if a.asname:
                    out[a.asname] = a.name
        elif isinstance(n, ast.ImportFrom) and n.module:
            for a in n.names:
                if a.name != "*":
                    out[a.asname or a.name] = n.module + "." + a.name
    return out


def _resolve(dotted: str, aliases: dict) -> str:
    """`dotted` with its first component replaced through the import table."""
    if not dotted:
        return dotted
    head, sep, rest = dotted.partition(".")
    if head in aliases:
        return aliases[head] + (sep + rest if sep else "")
    return dotted


def _py_marker_kind(expr, aliases=None) -> str | None:
    """'skip' / 'xfail' / None for a decorator or `pytestmark` element.

    Both the raw last name and the last name after alias resolution are
    checked, so `@nope('x')` (with `from unittest import skip as nope`) and
    `@m.skip` (with `from pytest import mark as m`) are both seen."""
    raw = _dotted(expr)
    lasts = {raw.rpartition(".")[2], _resolve(raw, aliases or {}).rpartition(".")[2]}
    if lasts & PY_SKIP_DECORATORS:
        return "skip"
    if lasts & PY_XFAIL_DECORATORS:
        return "xfail"
    return None


def _py_expr_markers(expr, aliases) -> tuple:
    """(skips, xfails) from every marker-shaped name anywhere in `expr` -- used
    for the value of a monkey-skip such as `T.test_a = unittest.skip('x')(T.test_a)`.
    Only maximal Name/Attribute chains are judged (`unittest.skip` once, not
    also its `unittest` sub-node)."""
    s = x = 0
    stack = [expr]
    while stack:
        n = stack.pop()
        if isinstance(n, (ast.Name, ast.Attribute)):
            k = _py_marker_kind(n, aliases)
            if k == "skip":
                s += 1
            elif k == "xfail":
                x += 1
            continue
        stack.extend(ast.iter_child_nodes(n))
    return s, x


def _py_body_markers(nodes, aliases=None) -> tuple:
    """(skips, xfails) from calls/raises anywhere inside `nodes`, NOT descending
    into nested function or class definitions (those are their own scope)."""
    aliases = aliases or {}
    skips = xfails = 0
    stack = list(nodes)
    while stack:
        n = stack.pop()
        if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef, ast.Lambda)):
            continue
        if isinstance(n, ast.Call):
            d = _resolve(_dotted(n), aliases)
            last = d.rpartition(".")[2]
            # `self.skipTest(...)`, `pytest.skip(...)`, `pytest.importorskip(...)`.
            if last == "skipTest" or (
                last in ("skip", "importorskip") and d.startswith("pytest")
            ):
                skips += 1
            elif last in PY_XFAIL_CALLS and d.startswith("pytest"):
                xfails += 1
        if isinstance(n, ast.Raise) and n.exc is not None:
            raw = _dotted(n.exc)
            if "SkipTest" in (raw.rpartition(".")[2], _resolve(raw, aliases).rpartition(".")[2]):
                skips += 1
        stack.extend(ast.iter_child_nodes(n))
    return skips, xfails


def _py_assert_count(nodes) -> int:
    total = 0
    for top in nodes:
        for n in ast.walk(top):
            if isinstance(n, ast.Assert):
                total += 1
            elif isinstance(n, ast.Call):
                d = _dotted(n)
                last = d.rpartition(".")[2]
                if last.startswith("assert") or d in ("pytest.raises", "pytest.fail"):
                    total += 1
    return total


def py_counts(src: str, path: str) -> dict:
    """Test-surface counts of a Python test file, in the same shape as
    `counts`, plus per-test skip/xfail maps (`fn_skips` / `fn_xfails`)."""

    if not src:
        return {"assertions": 0, "tests": 0, "ignores": 0, "should_panics": 0,
                "fn_assertions": {}, "fn_skips": {}, "fn_xfails": {}}
    try:
        with warnings.catch_warnings():
            warnings.simplefilter("ignore")
            tree = ast.parse(src, filename=path)
    except (SyntaxError, ValueError) as exc:
        raise Undetermined(f"{path}: Python test file does not parse: {exc}") from exc

    fn_assertions: dict = {}
    fn_skips: dict = {}
    fn_xfails: dict = {}
    aliases = _py_import_aliases(tree)
    # Skips applied from OUTSIDE the decorated definition: key is a test's
    # qualified name ("T.test_a") or a class prefix ("T."); value (skips, xfails).
    extra: dict = {}

    def add_extra(key: str, s: int, x: int) -> None:
        ps, px = extra.get(key, (0, 0))
        extra[key] = (ps + s, px + x)

    def disables(stmt) -> int:
        """1 if `stmt` assigns `__test__` anything but a literal True (pytest's
        switch that turns collection of the enclosing module/class off)."""
        if isinstance(stmt, (ast.Assign, ast.AnnAssign)):
            targets = stmt.targets if isinstance(stmt, ast.Assign) else [stmt.target]
            if any(isinstance(t, ast.Name) and t.id == "__test__" for t in targets):
                v = stmt.value
                return 0 if isinstance(v, ast.Constant) and v.value is True else 1
        return 0

    def markers_of(decorators) -> tuple:
        s = x = 0
        for dec in decorators:
            k = _py_marker_kind(dec, aliases)
            if k == "skip":
                s += 1
            elif k == "xfail":
                x += 1
        return s, x

    # Module-level skips cover every test in the file.
    mod_s = mod_x = 0
    module_stmts = []
    for stmt in tree.body:
        if isinstance(stmt, (ast.FunctionDef, ast.AsyncFunctionDef)) and stmt.name == "setUpModule":
            # A skip raised/called in setUpModule skips every test in the module.
            s, x = _py_body_markers(stmt.body, aliases)
            mod_s += s
            mod_x += x
            continue
        if isinstance(stmt, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
            continue
        mod_s += disables(stmt)
        # Monkey-skip: `T.test_a = unittest.skip('x')(T.test_a)`,
        # `test_a = pytest.mark.skip(test_a)`, `T = unittest.skip('x')(T)`,
        # `setattr(T, "test_a", unittest.skip('x')(T.test_a))`.
        mk_targets, mk_value = [], None
        if isinstance(stmt, ast.Assign):
            mk_targets = [_dotted(t) for t in stmt.targets]
            mk_value = stmt.value
        elif (isinstance(stmt, ast.Expr) and isinstance(stmt.value, ast.Call)
              and _dotted(stmt.value) == "setattr" and len(stmt.value.args) == 3
              and isinstance(stmt.value.args[1], ast.Constant)
              and isinstance(stmt.value.args[1].value, str)):
            mk_targets = [_dotted(stmt.value.args[0]) + "." + stmt.value.args[1].value]
            mk_value = stmt.value.args[2]
        if mk_value is not None:
            ms, mx = _py_expr_markers(mk_value, aliases)
            keys = []
            for d in mk_targets:
                if not d or d.startswith(".") or d in ("pytestmark", "__test__"):
                    continue
                # a test function gets its own key; anything else (a class)
                # covers every test under it.
                keys.append(d if d.rpartition(".")[2].startswith("test") else d + ".")
            if (ms or mx) and keys:
                for k in keys:
                    add_extra(k, ms, mx)
                continue  # attributed above; do not ALSO count it module-wide
        module_stmts.append(stmt)
        if isinstance(stmt, (ast.Assign, ast.AnnAssign)):
            targets = stmt.targets if isinstance(stmt, ast.Assign) else [stmt.target]
            if any(isinstance(t, ast.Name) and t.id == "pytestmark" for t in targets) and stmt.value is not None:
                vals = stmt.value.elts if isinstance(stmt.value, (ast.List, ast.Tuple)) else [stmt.value]
                s, x = markers_of(vals)
                mod_s += s
                mod_x += x
    s, x = _py_body_markers(module_stmts, aliases)
    mod_s += s
    mod_x += x

    def visit(body, prefix: str, inh_s: int, inh_x: int) -> None:
        for node in body:
            if isinstance(node, ast.ClassDef):
                s, x = markers_of(node.decorator_list)
                for item in node.body:
                    s += disables(item)  # class attribute `__test__ = False`
                    if (isinstance(item, (ast.FunctionDef, ast.AsyncFunctionDef))
                            and item.name in ("setUp", "setUpClass")):
                        bs, bx = _py_body_markers(item.body, aliases)
                        s += bs
                        x += bx
                visit(node.body, prefix + node.name + ".", inh_s + s, inh_x + x)
            elif isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and node.name.startswith("test"):
                name = prefix + node.name
                s, x = markers_of(node.decorator_list)
                bs, bx = _py_body_markers(node.body, aliases)
                es = ex = 0
                for key, (ks, kx) in extra.items():
                    if key == name or (key.endswith(".") and name.startswith(key)):
                        es += ks
                        ex += kx
                fn_assertions[name] = fn_assertions.get(name, 0) + _py_assert_count(node.body)
                fn_skips[name] = fn_skips.get(name, 0) + inh_s + s + bs + es
                fn_xfails[name] = fn_xfails.get(name, 0) + inh_x + x + bx + ex

    visit(tree.body, "", mod_s, mod_x)

    # Count test functions the same way `visit` finds them (module + classes).
    def count_tests(body) -> int:
        c = 0
        for node in body:
            if isinstance(node, ast.ClassDef):
                c += count_tests(node.body)
            elif isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef)) and node.name.startswith("test"):
                c += 1
        return c

    n_tests = count_tests(tree.body)
    return {
        "assertions": _py_assert_count(tree.body),
        "tests": n_tests,
        "ignores": 0,  # Python skips are judged per surviving test (fn_skips)
        "should_panics": 0,
        "fn_assertions": fn_assertions,
        "fn_skips": fn_skips,
        "fn_xfails": fn_xfails,
    }


def py_marker_gains(before: dict, after: dict, key: str) -> list:
    """`(name, before, after)` for every test present on BOTH sides whose
    skip/xfail count went up."""
    out = []
    for name, n_before in sorted(before[key].items()):
        n_after = after[key].get(name)
        if n_after is not None and n_after > n_before:
            out.append((name, n_before, n_after))
    return out


def py_findings_for(path: str, before: dict, after: dict) -> list:
    out = []
    losses = per_fn_losses(before["fn_assertions"], after["fn_assertions"])
    if after["assertions"] < before["assertions"] or losses:
        detail = "assertions {} -> {}".format(before["assertions"], after["assertions"])
        if losses:
            detail += "; weakened test fn(s): " + ", ".join(
                "{}() {} -> {}".format(n, b, a) for n, b, a in losses
            )
        out.append((path, KIND_ASSERTION_REMOVED, detail))
    if after["tests"] < before["tests"]:
        out.append((path, KIND_TEST_REMOVED,
                    "test functions {} -> {}".format(before["tests"], after["tests"])))
    for key, kind, label in (
        ("fn_skips", KIND_IGNORE_ADDED, "skip markers"),
        ("fn_xfails", KIND_SHOULD_PANIC_ADDED, "expected-failure markers"),
    ):
        gains = py_marker_gains(before, after, key)
        if gains:
            out.append((path, kind, "{} added to existing test(s): {}".format(
                label, ", ".join("{}() {} -> {}".format(n, b, a) for n, b, a in gains))))
    return out


# --- findings --------------------------------------------------------------


def per_fn_losses(before: dict, after: dict) -> list:
    """`(name, before, after)` for every `#[test]` fn that SURVIVED and lost
    assertions.

    Only names present on BOTH sides are compared. A name that is gone at HEAD is
    a deletion or a rename, and neither is an assertion loss inside a surviving
    test — see the module docstring for why a rename is deliberately not paired.
    """
    losses = []
    for name, n_before in sorted(before.items()):
        n_after = after.get(name)
        if n_after is not None and n_after < n_before:
            losses.append((name, n_before, n_after))
    return losses


def findings_for(path: str, before: dict, after: dict) -> list:
    out = []
    losses = per_fn_losses(before["fn_assertions"], after["fn_assertions"])
    if after["assertions"] < before["assertions"] or losses:
        detail = "assertion macros {} -> {}".format(
            before["assertions"], after["assertions"]
        )
        if losses:
            # Name ONLY the losing functions. Listing the whole file would
            # satisfy a reviewer's eye while telling them nothing.
            detail += "; weakened test fn(s): " + ", ".join(
                "{}() {} -> {}".format(n, b, a) for n, b, a in losses
            )
        out.append((KIND_ASSERTION_REMOVED, detail))
    if after["tests"] < before["tests"]:
        out.append(
            (
                KIND_TEST_REMOVED,
                "#[test] functions {} -> {}".format(before["tests"], after["tests"]),
            )
        )
    if after["ignores"] > before["ignores"]:
        out.append(
            (
                KIND_IGNORE_ADDED,
                "#[ignore] attributes {} -> {}".format(
                    before["ignores"], after["ignores"]
                ),
            )
        )
    if after["should_panics"] > before["should_panics"]:
        out.append(
            (
                KIND_SHOULD_PANIC_ADDED,
                "#[should_panic] attributes {} -> {}".format(
                    before["should_panics"], after["should_panics"]
                ),
            )
        )
    return [(path, kind, detail) for kind, detail in out]


# --- driver ----------------------------------------------------------------


def _is_rs(path) -> bool:
    return bool(path) and path.endswith(".rs")


# One whole line that is exactly an `#[ignore]` / `#[ignore = "reason"]`
# attribute (a reason without quotes or backslashes inside it), nothing else.
_IGNORE_LINE_RE = re.compile(
    r'[ \t]*#[ \t]*\[[ \t]*ignore[ \t]*(?:=[ \t]*"[^"\\\n]*"[ \t]*)?\][ \t]*\n?'
)
# The same line after `_mask_noncode`: the reason literal is blanked to spaces.
_IGNORE_LINE_MASKED_RE = re.compile(
    r"[ \t]*#[ \t]*\[[ \t]*ignore[ \t]*(?:=[ \t]*)?\][ \t]*\n?"
)


# Inserted code may not contain a (shadowing) template, a macro leaking out of
# a module, or an inner attribute `#![...]` (applies to the whole file/module);
# nor, at its own top level, a macro invocation (`name!`, not `!=`).
_INSERT_FORBIDDEN_RE = re.compile(r"\bmacro_rules\b|\bmacro_use\b|#[ \t\n]*!")
_ITEM_MACRO_RE = re.compile(r"\b[A-Za-z_][A-Za-z0-9_]*[ \t\n]*!(?!=)")


def _inserted_block_is_safe(ma: list, j1: int, j2: int) -> bool:
    """True only when the inserted MASKED after-lines `ma[j1:j2]` are whole
    top-level items that cannot touch an existing test: the block starts at
    bracket depth 0 over `{}[]()` (outside every `macro_rules!` template body,
    whatever its delimiter, and every fn/mod body); is balanced on its own
    (cannot wrap existing lines, e.g. `fn dead() {` ... `}` around
    invocations); follows and ends at an item boundary (`}`/`;`), so it splits
    no item and leaves no dangling `#[cfg(any())]` on the next invocation; and
    has none of `_INSERT_FORBIDDEN_RE` nor a top-level macro invocation, which
    could redefine the template so later invocations expand to nothing."""
    prev = [ln for ln in ma[:j1] if ln.strip()]
    if prev and not prev[-1].rstrip().endswith(("}", ";")):
        return False
    depth, top = 0, []
    for ch in "".join(ma[:j1]):
        depth += (ch in "{[(") - (ch in "}])")
    if depth != 0:
        return False
    block = "".join(ma[j1:j2])
    for ch in block:
        depth += (ch in "{[(") - (ch in "}])")
        if depth < 0:
            return False
        top.append(ch if depth == 0 and ch not in "}])" else " ")
    last = [ln for ln in ma[j1:j2] if ln.strip()]
    return (
        depth == 0
        and not _INSERT_FORBIDDEN_RE.search(block)
        and not _ITEM_MACRO_RE.search("".join(top))
        and not (last and not last[-1].rstrip().endswith(("}", ";")))
    )


def _only_proven_safe_line_changes(before: str, after: str, path: str) -> bool:
    """True only when `after` is `before` with whole `#[ignore...]` attribute
    lines deleted and/or blocks inserted that `_inserted_block_is_safe` proves
    are top-level items; every other line is byte-identical AND identical after
    masking (an insert cannot open a comment/literal swallowing existing code).
    No line may be edited or moved.

    These are the changes to a file holding a `macro_rules!` test template that
    are PROVEN not to remove or weaken a generated test: every invocation,
    template body and assertion survives byte for byte with the same meaning.
    Each deleted line must also be an ignore attribute in CODE -- the masked
    line must still read `#[ignore ...]` -- so deleting a look-alike inside a
    string literal or a comment does not qualify. An insertion inside a
    template (a `return;` before the asserts, a new `#[ignore]`) is not proven
    safe. Anything else returns False and the caller stays Undetermined. A
    tokenisation failure in `_mask_noncode` propagates as Undetermined.
    """
    b = before.splitlines(keepends=True)
    a = after.splitlines(keepends=True)
    mb = _mask_noncode(before, path).splitlines(keepends=True)
    ma = _mask_noncode(after, path).splitlines(keepends=True)
    if len(mb) != len(b) or len(ma) != len(a):
        return False
    sm = difflib.SequenceMatcher(None, b, a, autojunk=False)
    for tag, i1, i2, j1, j2 in sm.get_opcodes():
        if tag == "equal" and mb[i1:i2] == ma[j1:j2]:
            continue
        if tag == "insert" and _inserted_block_is_safe(ma, j1, j2):
            continue
        if tag != "delete":
            return False
        for k in range(i1, i2):
            if not _IGNORE_LINE_RE.fullmatch(b[k]):
                return False
            if not _IGNORE_LINE_MASKED_RE.fullmatch(mb[k]):
                return False
    return True


HEAD_MARKER_KINDS = (KIND_IGNORE_ADDED, KIND_SHOULD_PANIC_ADDED)


def head_marker_findings(rep: str, new: str, head_side: Side, after_side: Side) -> list:
    """Skip / expected-failure marker gains of `new` since HEAD, for a file that
    is absent at the merge-base (backlog 78817f03).

    Only the marker kinds (`HEAD_MARKER_KINDS`) are returned; assertion and
    test-count loss against HEAD is out of scope by the 2026-11-07 ruling. A
    path absent at HEAD (the file's first commit) yields nothing: a test that is
    skipped from birth hides no coverage that ever existed. A HEAD blob that is
    in the tree but unreadable raises Undetermined (via `blob_at`).

    A Rust `macro_rules!` test template is NOT Undetermined here, unlike the
    base comparison in `scan`: marker gains are counted from the textual
    `#[ignore]` / `#[should_panic]` attributes, which a template body carries
    verbatim, and a new invocation of an already-marked template is a new
    already-skipped test, the same shape as the first-commit pin above.
    """
    head_src = head_side.read(new)
    if head_src is None:
        return []
    after_src = after_side.read(new) or ""
    if is_py_test_file(new):
        found = py_findings_for(rep, py_counts(head_src, new), py_counts(after_src, new))
    else:
        found = findings_for(
            rep,
            counts(test_surface(head_src, new, head_side), new) if head_src else counts(""),
            counts(test_surface(after_src, new, after_side), new) if after_src else counts(""),
        )
    return [
        (p, kind, detail + " (since HEAD; file absent at the merge-base)")
        for p, kind, detail in found
        if kind in HEAD_MARKER_KINDS
    ]


def scan(repo: str, base: str, pending_msg=None) -> list:
    if not is_git_repo(repo):
        raise Undetermined(f"not a git repository: {repo}")
    base_sha = resolve_base(repo, base)
    merge_base = git(repo, "merge-base", base_sha, "HEAD").strip()
    if not merge_base:
        raise Undetermined("merge-base produced no commit")

    pairs = [
        (old, new)
        for (old, new) in changed_pairs(repo, merge_base)
        if _is_rs(old) or _is_rs(new) or is_py_test_file(old) or is_py_test_file(new)
    ]

    acked = acknowledgements(repo, merge_base, pending_msg)

    base_side = Side(
        f"base {merge_base[:12]}",
        lambda p: blob_or_none(repo, merge_base, p),
        lambda: grep_path_attr_files(repo, merge_base),
    )
    after_side = Side(
        "worktree",
        lambda p: worktree_or_none(repo, p),
        lambda: grep_path_attr_files(repo, None),
    )
    head_side = Side(
        "HEAD",
        lambda p: blob_or_none(repo, "HEAD", p),
        lambda: grep_path_attr_files(repo, "HEAD"),
    )

    # Out-of-line `#[cfg(test)] mod x;` body files are scanned as part of their
    # DECLARING file, on each side, never under their own path: that keeps a
    # verbatim inline -> file move equal, and keeps the body file from being
    # counted twice or read as production code. A changed body file whose
    # declarer did not change pulls the declarer into the scan.
    body_base = {}
    body_after = {}
    for old, new in pairs:
        if old:
            r = test_body_root(base_side, old)
            if r is not None:
                body_base[old] = r
        if new:
            r = test_body_root(after_side, new)
            if r is not None:
                body_after[new] = r
    touched = {p for pr in pairs for p in pr if p}
    for r in list(body_base.values()) + list(body_after.values()):
        if r not in touched:
            pairs.append((r, r))
            touched.add(r)

    results = []
    for old, new in sorted(set(pairs), key=lambda pr: (pr[1] or "", pr[0] or "")):
        # Remember whether the old side was absent in the original pair,
        # before reassignments for out-of-line cfg(test) body files.
        old_is_new = old is None
        if old in body_base:
            old = None
        if new in body_after:
            new = None
        if old is None and new is None:
            continue
        rep = new or old  # attribute the finding to the surviving/current path
        before_src = blob_at(repo, merge_base, old) if old else ""
        after_src = worktree_content(repo, new) if new else ""
        if is_py_test_file(old) or is_py_test_file(new):
            # Python side(s). A side that is not a Python test file (a rename to
            # a non-test name, or an `.rs` counterpart) contributes an EMPTY
            # surface, so moving tests out of the scanned set reads as removal.
            before = py_counts(before_src if is_py_test_file(old) else "", old or "")
            after = py_counts(after_src if is_py_test_file(new) else "", new or "")
            found = py_findings_for(rep, before, after)
        else:
            before = (
                counts(test_surface(before_src, old, base_side), old)
                if before_src else counts("")
            )
            after = (
                counts(test_surface(after_src, new, after_side), new)
                if after_src else counts("")
            )
            if (
                before_src
                and (before["template_tests"] or after["template_tests"])
                and not (
                    old == new
                    and _only_proven_safe_line_changes(before_src, after_src, rep)
                    and _only_proven_safe_line_changes(
                        test_surface(before_src, old, base_side),
                        test_surface(after_src, new, after_side),
                        rep,
                    )
                )
            ):
                # A `macro_rules!` test template expands once per invocation,
                # and invocations are not counted: dropping `t!(case, ...)` or
                # turning explicit tests into a template removes tests with no
                # textual `#[test]` / assertion change. Against a base where
                # the file existed we cannot judge that. Only a file with no
                # base (`before` all zero, so no finding can exist) is judged,
                # plus one proven-safe shape: the same path whose whole file AND
                # whose test surface (which includes out-of-line `#[cfg(test)]`
                # bodies) differ from the base only by deleted whole-line
                # `#[ignore...]` attributes in code and/or inserted whole
                # top-level items proven to lie outside every template and fn
                # body (`_only_proven_safe_line_changes`). Every invocation and
                # template body is then byte-identical with the same meaning,
                # so no generated test can have been removed or weakened; such
                # a change falls through to the ordinary findings below.
                raise Undetermined(
                    f"{rep}: #[test] inside a macro_rules! template in a file "
                    "that exists at the base -- macro invocations are not "
                    "counted, so removed generated tests cannot be ruled out"
                )
            found = findings_for(rep, before, after)
        # A file that did not exist at the base cannot have had existing
        # coverage weakened. Filter out ignore-added findings when the file is
        # new (old_is_new), as these cannot indicate coverage loss in a file
        # with no prior tests.
        if old_is_new:
            found = [
                (p, kind, detail) for p, kind, detail in found
                if kind != KIND_IGNORE_ADDED
            ]
            # ...but a file absent at the base may already exist at HEAD, from an
            # earlier commit of this branch, and its tests then DID exist. For
            # such a file, a skip / expected-failure marker gained since HEAD is
            # judged (backlog 78817f03, user ruling 2026-11-07). Assertion and
            # test-count loss against HEAD is deliberately NOT judged: it fires
            # on ordinary refactoring of a test still being written.
            if new is not None:
                have = {(p, kind) for p, kind, _ in found}
                for p, kind, detail in head_marker_findings(rep, new, head_side, after_side):
                    if (p, kind) not in have:
                        found.append((p, kind, detail))
        for p, kind, detail in found:
            results.append(
                {
                    "path": p,
                    "kind": kind,
                    "detail": detail,
                    "acknowledged": (p, kind) in acked,
                }
            )
    return results


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument(
        "--base",
        default="main",
        help="the branch this worktree integrates into (default: local main); "
        "the scan covers merge-base(BASE, HEAD) to the worktree. Not origin/main: "
        "a stale remote ref charges main's commits to the branch (#1181)",
    )
    ap.add_argument("--repo", default=None)
    ap.add_argument("--json", action="store_true")
    ap.add_argument(
        "--pending-msg",
        default=None,
        metavar="FILE",
        help="commit message about to be written (a commit-msg hook's $1); "
        "scanned for acknowledgments IN ADDITION to base..HEAD",
    )
    args = ap.parse_args(argv)

    repo = args.repo or os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    try:
        findings = scan(repo, args.base, args.pending_msg)
    except Undetermined as exc:
        # The one branch that must never become 0. `undetermined` is greppable
        # on purpose so a CI log can be searched for it.
        print(f"check-test-weakening: undetermined — {exc}", file=sys.stderr)
        if args.json:
            print(json.dumps({"verdict": "undetermined", "findings": []}))
        return 2

    unacked = [f for f in findings if not f["acknowledged"]]
    verdict = "weakened" if unacked else "clean"

    if args.json:
        print(json.dumps({"verdict": verdict, "findings": findings}))
    else:
        for f in findings:
            mark = "ack" if f["acknowledged"] else "BLOCK"
            print(f"[{mark}] {f['path']}: {f['kind']} ({f['detail']})")
        if unacked:
            print(
                "\ncheck-test-weakening: {} unacknowledged weakening finding(s).\n"
                "If a deletion is genuinely obsolete, say so in the commit message:\n"
                "  test-weakening-justified: <path>:<kind> - <reason>".format(
                    len(unacked)
                ),
                file=sys.stderr,
            )
        else:
            print("check-test-weakening: no unacknowledged test weakening.")

    return 1 if unacked else 0


if __name__ == "__main__":
    sys.exit(main())
