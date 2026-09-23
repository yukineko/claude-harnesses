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
read or decoded, or a `#[cfg(test)]` module whose braces do not balance (an
unparsed module is an uninspected one). None of these may collapse into 0: an
unreadable test surface is not a clean test surface.

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

* The marker is read from `base..HEAD` commit messages AND, when `--pending-msg
  FILE` is passed, from the message about to be written. Until 2026-07-30 only
  the former was read, which made the hatch unreachable exactly when it was
  needed: git runs `pre-commit` before any message exists, and on a branch's
  first commit `base..HEAD` is empty, so a staged weakening could not be
  acknowledged at all and `--no-verify` was the only way past a false positive.
  The justification is still durable in history where a reviewer meets it --
  `--pending-msg` points at the message that is being committed, not at a
  scratch file -- and it is held to the same exactness rules. This gate now runs
  from .githooks/commit-msg, where git supplies that path as $1.
* Scope is base vs HEAD, not per commit. A file that did not exist at the base
  cannot have had existing coverage weakened, and per-commit analysis would fire
  on ordinary iteration (write test, refactor, consolidate); a gate that fires on
  normal work gets switched off. The gap this leaves -- adding a strong new test
  inside a PR and gutting it before merge -- is covered by a different control,
  the tdd F->P oracle, which requires a RED observation before the GREEN and so
  leaves a proof trail this scanner need not duplicate.
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
  knowing it is code; the per-file counts still read the raw surface, so this
  change does not move any existing verdict. It also means an `assert!` that
  appears only inside a string fixture is not credited to the enclosing test.

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
import json
import os
import re
import subprocess
import sys

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
# attribute to the body it belongs to (see `test_fn_assertions`).
FN_DECL_RE = re.compile(r"\bfn\s+([A-Za-z_][A-Za-z0-9_]*)")
# Everything Rust permits between an attribute and the `fn` it applies to:
# further attributes, visibility, and the asyncness/constness/ABI qualifiers.
# Anything else means the `#[test]` we matched is not that function's attribute.
ITEM_PREFIX_RE = re.compile(
    r"\A(?:\s|#\s*\[[^\[\]]*\]|pub(?:\s*\([^()]*\))?|async|unsafe|const"
    r'|extern(?:\s*"[^"]*")?)*\Z'
)

KIND_ASSERTION_REMOVED = "assertion-removed"
KIND_TEST_REMOVED = "test-removed"
KIND_IGNORE_ADDED = "ignore-added"
KIND_SHOULD_PANIC_ADDED = "should-panic-added"

ALL_KINDS = (
    KIND_ASSERTION_REMOVED,
    KIND_TEST_REMOVED,
    KIND_IGNORE_ADDED,
    KIND_SHOULD_PANIC_ADDED,
)

# The separator must be a dash FENCED BY WHITESPACE, and the id is matched
# greedily. Both details matter: the id is `<path>:<kind>`, so it contains `:`
# and `-` itself. A non-greedy id with `:` allowed as a separator stops at the
# first colon, silently reducing the id to the bare path and acknowledging
# nothing — the marker looks accepted while the finding still blocks.
JUSTIFY_RE = re.compile(
    r"^\s*test-weakening-justified\s*:\s*(?P<id>\S+)\s+[-—–]+\s+(?P<reason>\S.*)$",
)


class Undetermined(Exception):
    """The verdict could not be established. Always resolves to exit 2."""


# --- git plumbing ----------------------------------------------------------


def git(repo: str, *args: str, allow_fail: bool = False) -> str:
    """Run git, treating every failure mode as undetermined unless allowed.

    `allow_fail` is only for probes whose non-zero exit is itself an ANSWER
    (e.g. `rev-parse --verify` on a ref that may legitimately not exist); it
    returns "" in that case. Everything else raises, because a checker that
    did not run is not a checker that passed.
    """
    try:
        proc = subprocess.run(
            ("git", "-C", repo) + args,
            capture_output=True,
            text=True,
        )
    except OSError as exc:
        raise Undetermined(f"could not run git: {exc}") from exc
    if proc.returncode != 0:
        if allow_fail:
            return ""
        raise Undetermined(
            "git {} exited {}: {}".format(
                " ".join(args), proc.returncode, proc.stderr.strip()
            )
        )
    return proc.stdout


def resolve_base(repo: str, base: str, base_was_explicit: bool) -> str:
    """Resolve the comparison base to a commit sha.

    The default (`origin/main`) may legitimately be absent in a clone that has
    no remote, so it falls back to `main`. An EXPLICIT `--base` gets no such
    fallback: silently comparing against something the caller did not ask for
    would produce a confident verdict about the wrong range.
    """
    candidates = [base] if base_was_explicit else [base, "main"]
    for cand in candidates:
        out = git(repo, "rev-parse", "--verify", "--quiet", cand + "^{commit}",
                  allow_fail=True).strip()
        if out:
            return out
    raise Undetermined(f"base ref does not resolve: {base}")


def is_git_repo(repo: str) -> bool:
    if not os.path.isdir(repo):
        return False
    out = git(repo, "rev-parse", "--is-inside-work-tree", allow_fail=True).strip()
    return out == "true"


# --- test-surface extraction ----------------------------------------------


def is_tests_dir_file(path: str) -> bool:
    parts = path.replace("\\", "/").split("/")
    return path.endswith(".rs") and "tests" in parts[:-1]


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


def cfg_test_modules(src: str, path: str) -> str:
    """Concatenate every `#[cfg(test)]` module body found in `src`.

    Brace matching skips string/char literals and comments (see
    `_matching_brace`); if a module's braces still do not balance we cannot claim
    to have inspected it, so the whole run goes undetermined rather than
    reporting on a partial read.
    """
    chunks = []
    for m in re.finditer(r"#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]", src):
        brace = src.find("{", m.end())
        if brace == -1:
            raise Undetermined(f"{path}: #[cfg(test)] with no module body")
        end = _matching_brace(src, brace)
        if end is None:
            raise Undetermined(f"{path}: unbalanced braces in #[cfg(test)] module")
        chunks.append(src[brace : end + 1])
    return "\n".join(chunks)


def test_surface(src: str, path: str) -> str:
    if is_tests_dir_file(path):
        return src
    return cfg_test_modules(src, path)


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
    return {
        "assertions": len(ASSERT_RE.findall(surface)),
        "tests": len(TEST_ATTR_RE.findall(surface)),
        "ignores": len(IGNORE_ATTR_RE.findall(surface)),
        "should_panics": len(SHOULD_PANIC_RE.findall(surface)),
        "fn_assertions": test_fn_assertions(surface, path),
    }


# --- file contents on both sides ------------------------------------------


def blob_at(repo: str, rev: str, path: str) -> str:
    """Content of `path` at `rev`, or "" when it genuinely did not exist there.

    Existence is settled against the TREE first, and only a real absence maps to
    "". Deriving absence from `git show`'s exit code instead would swallow every
    other failure — a corrupt object, an unreadable pack — as "the file is new".
    That is not a harmless mistake here: an empty base surface makes
    `after < before` unsatisfiable, so no weakening in that file is detectable
    and the run reports `clean`. Confirmed by fault injection, not argued:
    corrupting the base blob flipped an `assertion-removed` finding (exit 1)
    into `{"verdict": "clean"}` (exit 0) with nothing on stderr.
    """
    listed = git(repo, "ls-tree", "-r", "--name-only", rev, "--", path)
    if not listed.strip():
        return ""  # absent at `rev`: the file is new, which is a real answer
    try:
        proc = subprocess.run(
            ("git", "-C", repo, "show", f"{rev}:{path}"),
            capture_output=True,
        )
    except OSError as exc:
        raise Undetermined(f"could not run git show: {exc}") from exc
    if proc.returncode != 0:
        # The tree says the blob is there, so a read failure is a failure to
        # observe, not an absence.
        raise Undetermined(
            "{}@{}: present in the tree but unreadable (git show exited {}): {}".format(
                path, rev, proc.returncode, proc.stderr.decode("utf-8", "replace").strip()
            )
        )
    try:
        return proc.stdout.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise Undetermined(f"{path}@{rev}: not valid UTF-8: {exc}") from exc


def worktree_content(repo: str, path: str) -> str:
    full = os.path.join(repo, path)
    if not os.path.exists(full):
        return ""  # deleted
    try:
        with open(full, "r", encoding="utf-8") as fh:
            return fh.read()
    except (OSError, UnicodeDecodeError) as exc:
        raise Undetermined(f"{path}: unreadable in the worktree: {exc}") from exc


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


def acknowledgements(repo: str, base_sha: str, pending_msg: str | None = None) -> set:
    """`<path>:<kind>` pairs acknowledged by commit messages in the range.

    A line without a well-formed `<path>:<kind>` and a non-empty reason is
    ignored on purpose, so a bare marker cannot act as a blanket pass.

    `pending_msg` is the path to a commit message that is about to be written —
    the `$1` a `commit-msg` hook receives. It is scanned in ADDITION to the
    range, never instead of it, and it is held to exactly the same exactness
    rules; it widens WHERE a marker may live, not HOW loose one may be.

    Without it the escape hatch is unreachable at the moment it is needed. git
    runs `pre-commit` before any message exists, and on a branch's first commit
    `base..HEAD` is empty, so a marker in the message being written is invisible
    and the only way past a false positive is `--no-verify` — the bypass this
    repo has a separate hook to refuse. A gate whose documented remedy cannot be
    applied is not strict, it is broken.
    """
    body = git(repo, "log", "--format=%B", f"{base_sha}..HEAD")
    if pending_msg is not None:
        try:
            with open(pending_msg, encoding="utf-8", errors="replace") as fh:
                body += "\n" + fh.read()
        except OSError as exc:
            # "could not read it" is not "it contained no markers". Reading the
            # first as the second would silently drop every acknowledgment the
            # caller believed they had written.
            raise Undetermined(f"pending commit message unreadable ({pending_msg}): {exc}")
    acked = set()
    for line in body.splitlines():
        m = JUSTIFY_RE.match(line)
        if not m:
            continue
        ident = m.group("id")
        if ":" not in ident:
            continue
        path, _, kind = ident.rpartition(":")
        if path and kind in ALL_KINDS:
            acked.add((path, kind))
    return acked


# --- driver ----------------------------------------------------------------


def _is_rs(path) -> bool:
    return bool(path) and path.endswith(".rs")


def changed_pairs(repo: str, merge_base: str) -> list:
    """`(old_path, new_path)` for every change between `merge_base` and the worktree.

    Two git flags carry the fail-closed contract here:

    * `--name-status -z` emits LITERAL, NUL-delimited paths. The default
      `--name-only` renders a non-ASCII path through `core.quotePath` as
      `"crates/…/\\343\\203\\206.rs"` -- a string ending in `"`, not `.rs`, so an
      `endswith('.rs')` filter drops the file and any weakening in it passes
      unseen. A path the scanner cannot even name is the purest cannot-determine.
    * `-M` makes a rename a single record whose OLD side is known, so a rename
      that also drops assertions is compared against the coverage it moved FROM.
      Without it, default rename detection reports only the destination, which
      then looks like a brand-new file (absent at base) and its lost coverage is
      invisible -- a weakening laundered through a `git mv`.

    `old_path` is None for an addition; `new_path` is None for a deletion.
    """
    raw = git(
        repo, "-c", "core.quotePath=false",
        "diff", "--name-status", "-z", "-M", merge_base,
    )
    toks = raw.split("\0")
    pairs = []
    i = 0
    while i < len(toks):
        status = toks[i]
        if status == "":
            i += 1
            continue
        code = status[0]
        if code in ("R", "C"):
            if i + 2 >= len(toks):
                raise Undetermined(f"truncated {code} record in diff output")
            pairs.append((toks[i + 1], toks[i + 2]))
            i += 3
        else:
            if i + 1 >= len(toks):
                raise Undetermined(f"truncated '{code}' record in diff output")
            path = toks[i + 1]
            if code == "A":
                pairs.append((None, path))
            elif code == "D":
                pairs.append((path, None))
            else:  # M, T, U -- and any future code -- same path on both sides
                pairs.append((path, path))
            i += 2
    return pairs


def scan(repo: str, base: str, base_was_explicit: bool, pending_msg=None) -> list:
    if not is_git_repo(repo):
        raise Undetermined(f"not a git repository: {repo}")
    base_sha = resolve_base(repo, base, base_was_explicit)
    merge_base = git(repo, "merge-base", base_sha, "HEAD").strip()
    if not merge_base:
        raise Undetermined("merge-base produced no commit")

    pairs = [
        (old, new)
        for (old, new) in changed_pairs(repo, merge_base)
        if _is_rs(old) or _is_rs(new)
    ]

    acked = acknowledgements(repo, merge_base, pending_msg)

    results = []
    for old, new in sorted(set(pairs), key=lambda pr: (pr[1] or "", pr[0] or "")):
        rep = new or old  # attribute the finding to the surviving/current path
        before_src = blob_at(repo, merge_base, old) if old else ""
        after_src = worktree_content(repo, new) if new else ""
        before = (
            counts(test_surface(before_src, old), old) if before_src else counts("")
        )
        after = counts(test_surface(after_src, new), new) if after_src else counts("")
        for p, kind, detail in findings_for(rep, before, after):
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
    ap.add_argument("--base", default="origin/main")
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
    base_was_explicit = args.base != "origin/main"

    try:
        findings = scan(repo, args.base, base_was_explicit, args.pending_msg)
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
