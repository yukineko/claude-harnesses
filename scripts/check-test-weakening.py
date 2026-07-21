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

* A weakening that is only STAGED cannot be acknowledged, because the marker
  lives in a commit message that does not exist yet. Run locally it therefore
  blocks until the justifying commit is written. That is the intent: the
  justification has to be durable in history where a reviewer meets it, not a
  transient state of someone's index.
* Scope is NET, PER FILE, base vs HEAD -- not per commit. A file that did not
  exist at the base cannot have had existing coverage weakened, and per-commit
  analysis would fire on ordinary iteration (write test, refactor, consolidate);
  a gate that fires on normal work gets switched off. The gap this leaves --
  adding a strong new test inside a PR and gutting it before merge -- is covered
  by a different control, the tdd F->P oracle, which requires a RED observation
  before the GREEN and so leaves a proof trail this scanner need not duplicate.
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


def cfg_test_modules(src: str, path: str) -> str:
    """Concatenate every `#[cfg(test)]` module body found in `src`.

    Brace matching is intentionally strict: if the braces of a module do not
    balance we cannot claim to have inspected it, so the whole run goes
    undetermined rather than reporting on a partial read.
    """
    chunks = []
    for m in re.finditer(r"#\s*\[\s*cfg\s*\(\s*test\s*\)\s*\]", src):
        brace = src.find("{", m.end())
        if brace == -1:
            raise Undetermined(f"{path}: #[cfg(test)] with no module body")
        depth = 0
        i = brace
        while i < len(src):
            if src[i] == "{":
                depth += 1
            elif src[i] == "}":
                depth -= 1
                if depth == 0:
                    chunks.append(src[brace : i + 1])
                    break
            i += 1
        else:
            raise Undetermined(f"{path}: unbalanced braces in #[cfg(test)] module")
    return "\n".join(chunks)


def test_surface(src: str, path: str) -> str:
    if is_tests_dir_file(path):
        return src
    return cfg_test_modules(src, path)


def counts(surface: str) -> dict:
    return {
        "assertions": len(ASSERT_RE.findall(surface)),
        "tests": len(TEST_ATTR_RE.findall(surface)),
        "ignores": len(IGNORE_ATTR_RE.findall(surface)),
        "should_panics": len(SHOULD_PANIC_RE.findall(surface)),
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


def findings_for(path: str, before: dict, after: dict) -> list:
    out = []
    if after["assertions"] < before["assertions"]:
        out.append(
            (
                KIND_ASSERTION_REMOVED,
                "assertion macros {} -> {}".format(
                    before["assertions"], after["assertions"]
                ),
            )
        )
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


def acknowledgements(repo: str, base_sha: str) -> set:
    """`<path>:<kind>` pairs acknowledged by commit messages in the range.

    A line without a well-formed `<path>:<kind>` and a non-empty reason is
    ignored on purpose, so a bare marker cannot act as a blanket pass.
    """
    body = git(repo, "log", "--format=%B", f"{base_sha}..HEAD")
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


def scan(repo: str, base: str, base_was_explicit: bool) -> list:
    if not is_git_repo(repo):
        raise Undetermined(f"not a git repository: {repo}")
    base_sha = resolve_base(repo, base, base_was_explicit)
    merge_base = git(repo, "merge-base", base_sha, "HEAD").strip()
    if not merge_base:
        raise Undetermined("merge-base produced no commit")

    changed = [
        p
        for p in git(repo, "diff", "--name-only", merge_base).splitlines()
        if p.endswith(".rs")
    ]

    acked = acknowledgements(repo, merge_base)

    results = []
    for path in sorted(set(changed)):
        before_src = blob_at(repo, merge_base, path)
        after_src = worktree_content(repo, path)
        before = counts(test_surface(before_src, path)) if before_src else counts("")
        after = counts(test_surface(after_src, path)) if after_src else counts("")
        for p, kind, detail in findings_for(path, before, after):
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
    args = ap.parse_args(argv)

    repo = args.repo or os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    base_was_explicit = args.base != "origin/main"

    try:
        findings = scan(repo, args.base, base_was_explicit)
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
