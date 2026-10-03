#!/usr/bin/env python3
"""Round-2 RED repros for backlog 5e5cf0a9 (D1, D2, D6, D7).

D2 branch namespacing rule, derived from crates/condukt/src/worktree.rs
`namespaced_branch` (create side, applied whenever `--run` is given):
    branch = "<prefix>/<last>"  ->  "<prefix>/<run>/<last>"   (split at LAST '/')
    branch = "<name>"           ->  "<run>/<name>"
so `--run r1 --branch condukt/r1/run` really creates `condukt/r1/r1/run`, and
`--run r1 --branch condukt/t1` creates `condukt/r1/t1`.

The behavioural test builds nothing itself: it runs the condukt binary at
$CONDUKT_BIN (default: the scratchpad build named in the task). A missing
binary is a FAILURE, not a skip (cannot determine -> not green).
"""
from __future__ import annotations

import os
import re
import subprocess
import tempfile
import unittest

SCRIPTS = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(SCRIPTS)
SKILL = os.path.join(REPO, "crates", "condukt", "skills", "condukt", "SKILL.md")
WORKER = os.path.join(REPO, "crates", "condukt", "agents", "condukt-worker.md")
BIN = os.environ.get(
    "CONDUKT_BIN",
    "/private/tmp/claude-502/-Users-yuki-src-harness/dc66e988-2d93-4c9e-84f8-d20aa407c038/"
    "scratchpad/serialwt-target/debug/condukt",
)


def read(p: str) -> str:
    with open(p, encoding="utf-8") as f:
        return f.read()


def lineno(text: str, pos: int) -> int:
    return text.count("\n", 0, pos) + 1


# ---------------------------------------------------------------- D2 parsing
# An invocation runs from "condukt worktree create" to the first ')' / backtick
# / blank line / end of fence, so line-wrapped arguments are included.
_CREATE = re.compile(r"condukt\s+worktree\s+create\b(?P<args>[^)`]*?)(?=\)|`|\n\s*\n|\Z)", re.S)
_RUN = re.compile(r"--run\s+\S+")
_BRANCH = re.compile(r"--branch\s+(?P<v>[^\s)`]+)")
_RIDVAR = re.compile(r"\$\{?RID\}?")


def create_invocations(text: str) -> list[dict]:
    out = []
    for m in _CREATE.finditer(text):
        a = m.group("args")
        b = _BRANCH.search(a)
        out.append(
            {
                "line": lineno(text, m.start()),
                "has_run": bool(_RUN.search(a)),
                "branch": b.group("v").strip('"\'') if b else None,
                "end": m.end(),
            }
        )
    return out


def double_ns_hits(label: str, text: str) -> list[str]:
    return [
        f"{label}:{i['line']}: --run given but --branch {i['branch']} already contains the run id"
        for i in create_invocations(text)
        if i["has_run"] and i["branch"] and _RIDVAR.search(i["branch"])
    ]


def namespaced(run: str, branch: str) -> str:
    """Mirror of worktree.rs namespaced_branch (see module docstring)."""
    if "/" in branch:
        prefix, last = branch.rsplit("/", 1)
        return f"{prefix}/{run}/{last}"
    return f"{run}/{branch}"


def subst(s: str, rid: str = "r1", tid: str = "t1") -> str:
    s = _RIDVAR.sub(rid, s)
    s = s.replace("<k>", "1")
    return re.sub(r"<(?:t\.id|id)>", tid, s)


class D2Controls(unittest.TestCase):
    def test_positive(self):
        t = 'WP=$(condukt worktree create --run "$RID" --topic run \\\n  --branch condukt/$RID/run)'
        self.assertEqual(len(double_ns_hits("x", t)), 1)
        self.assertEqual(double_ns_hits("x", t)[0].split(":")[1], "1")

    def test_negative(self):
        t = 'WP=$(condukt worktree create --run "$RID" --topic <t.id> --branch condukt/<t.id>)'
        self.assertEqual(double_ns_hits("x", t), [])
        # no --run: legacy layout, RID in branch is not double namespacing
        self.assertEqual(
            double_ns_hits("x", "condukt worktree create --topic a --branch condukt/$RID/a"), []
        )

    def test_namespacing_mirror(self):
        self.assertEqual(namespaced("r1", "condukt/t1"), "condukt/r1/t1")
        self.assertEqual(namespaced("r1", "condukt/r1/run"), "condukt/r1/r1/run")
        self.assertEqual(namespaced("r1", "x"), "r1/x")


class D2StaticDoubleNamespace(unittest.TestCase):
    def test_no_branch_already_carries_run_id(self):
        hits = double_ns_hits("SKILL.md", read(SKILL)) + double_ns_hits(
            "condukt-worker.md", read(WORKER)
        )
        self.assertEqual(hits, [], f"{len(hits)} double-namespaced create(s):\n" + "\n".join(hits))


# ---------------------------------------------------- D2 behavioural (binary)
_STATE_BRANCH = re.compile(r"state\s+set\b[^\n]*?--branch\s+([^\s`)]+)")
_MERGE_BRANCH = re.compile(r"worktree\s+merge\s+--branch\s+([^\s`)]+)")


def doc_expectations(text: str, inv: dict) -> tuple[list[str], list[str]]:
    """(state-set branches within 12 lines after the create, merge branches in
    the doc sharing the create's last path segment) -- all with RID=r1,t.id=t1."""
    window = text[inv["end"]:]
    window = "\n".join(window.split("\n")[:12])
    states = [subst(x) for x in _STATE_BRANCH.findall(window)]
    last = subst(inv["branch"]).rsplit("/", 1)[-1]
    merges = [subst(x) for x in _MERGE_BRANCH.findall(text) if subst(x).rsplit("/", 1)[-1] == last]
    return states, merges


class D2Behavioural(unittest.TestCase):
    def _create(self, branch: str) -> str:
        """Run the real binary; return the branch ref that actually exists."""
        with tempfile.TemporaryDirectory() as d:
            repo = os.path.join(d, "repo")
            os.makedirs(repo)
            env = dict(os.environ, CONDUKT_WORKTREE_BASE=os.path.join(d, "wt"),
                       GIT_AUTHOR_NAME="a", GIT_AUTHOR_EMAIL="a@a", GIT_COMMITTER_NAME="a",
                       GIT_COMMITTER_EMAIL="a@a", CONDUKT_DEFAULT_BRANCH="main")
            g = lambda *a: subprocess.run(["git", "-C", repo, *a], env=env, check=True,
                                          capture_output=True, text=True).stdout
            g("init", "-q", "-b", "main")
            g("commit", "-q", "--allow-empty", "-m", "i")
            before = set(g("for-each-ref", "--format=%(refname:short)", "refs/heads").split())
            r = subprocess.run([BIN, "worktree", "create", "--run", "r1", "--topic", "tp",
                                "--branch", branch], cwd=repo, env=env, capture_output=True, text=True)
            self.assertEqual(r.returncode, 0, f"binary failed: {r.stderr}")
            new = set(g("for-each-ref", "--format=%(refname:short)", "refs/heads").split()) - before
            self.assertEqual(len(new), 1, f"expected exactly one new branch, got {new}")
            return new.pop()

    def test_binary_present(self):
        self.assertTrue(os.access(BIN, os.X_OK), f"condukt binary missing: {BIN}")

    def test_control_binary_matches_mirror(self):
        self.assertEqual(self._create("condukt/t1"), "condukt/r1/t1")

    def test_created_branch_equals_branch_skill_uses_later(self):
        text = read(SKILL)
        bad, checked = [], 0
        for inv in create_invocations(text):
            if not (inv["has_run"] and inv["branch"]):
                continue
            checked += 1
            actual = self._create(subst(inv["branch"]))
            states, merges = doc_expectations(text, inv)
            for s in states:
                if s != actual:
                    bad.append(f"SKILL.md:{inv['line']}: created {actual!r} but state set --branch uses {s!r}")
            if merges and actual not in merges:
                bad.append(f"SKILL.md:{inv['line']}: created {actual!r} but worktree merge uses {merges}")
        self.assertGreater(checked, 0, "no --run create invocations parsed (vacuous)")
        self.assertEqual(bad, [], f"{len(bad)} mismatch(es):\n" + "\n".join(bad))


# ------------------------------------------------------------------- D1 / D6
_SKIP = r"(?:スキップ|省略)(?!しない|は?しない|せず)"
_SINGLE = r"(?:単一\s*worktree|single[- ]worktree|fast[- ]path|ファストパス)"
D1_PATTERNS = [
    re.compile(rf"{_SINGLE}[^。]{{0,80}}?merge[^。]{{0,60}}?{_SKIP}"),
    re.compile(rf"{_SINGLE}[^。]{{0,80}}?{_SKIP}[^。]{{0,40}}?merge"),
    re.compile(r"commit[^。]{0,40}既定ブランチ上"),
    re.compile(r"既定ブランチ上[^。]{0,20}(?:commit|コミット)"),
]


def d1_hits(text: str) -> list[tuple[int, str]]:
    seen = {}
    for rx in D1_PATTERNS:
        for m in rx.finditer(text):
            seen.setdefault(lineno(text, m.start()), re.sub(r"\s+", " ", m.group(0)))
    return sorted(seen.items())


class D1D6Controls(unittest.TestCase):
    def test_d1_positive(self):
        self.assertTrue(d1_hits("単一 worktree モード（x）ではこの merge/remove ブロックを丸ごとスキップする"))
        self.assertTrue(d1_hits("単一 worktree\nモードでは merge を省略する"))
        self.assertTrue(d1_hits("（commit は既に既定ブランチ上にあり）"))

    def test_d1_negative(self):
        self.assertEqual(d1_hits("**Phase 7 の worktree merge/remove は省略しない**"), [])
        self.assertEqual(d1_hits("単一 worktree モードでは 1 度だけ merge し、スキップしない。"), [])
        self.assertEqual(d1_hits("統合までは既定ブランチに届かない"), [])


class D1SkipMerge(unittest.TestCase):
    def test_no_single_worktree_skip_merge_or_default_branch_claim(self):
        hits = [f"SKILL.md:{l}: {t}" for l, t in d1_hits(read(SKILL))]
        self.assertEqual(hits, [], f"{len(hits)} hit(s):\n" + "\n".join(hits))


# SUPERSEDED by USER RULING 2026-10-03 (backlog 5e5cf0a9, round 3): the shared
# run-branch design is retired, so the former D6 test (Phase 7 must merge
# `condukt/$RID/run`) and the D7 tests (`no-stage-no-commit` must be defined in
# condukt-worker.md) are deleted -- they pinned a design the ruling removed.
# The replacement contract lives in scripts/test_backlog_5e5cf0a9_r3.py.


if __name__ == "__main__":
    unittest.main()
