#!/usr/bin/env python3
"""Pre-commit gate: a backlog row may only become terminal WITH observed evidence.

Rule (user-ratified 2026-10-01): suspicion is never evidence. A terminal
transition (`done`, `cancelled`) needs an EXECUTED, COMMITTED test, a doc-only
commit, a duplicate target, or a human TTY ruling. The backlog CLI records that
evidence as a `[task.closure]` table; this gate is the commit-time check that
the table is well formed AND that its tests really behave as recorded. It
re-runs them: shape alone is a claim, and a claim is not an observation.

What is judged
--------------
`.backlog/tasks.toml` and `.backlog/tasks.done.toml` are read from HEAD and
from the INDEX (exactly the content being committed, never the working tree).
Rows are keyed by `id` across both files, so a row moving between the files is
one row. A staged row is judged when its status is terminal and
  * no HEAD row with that id was terminal (newly terminal), or
  * its (status, closure) differs from every HEAD row with that id (the
    closure was added, removed, or edited on an already-terminal row).
A terminal row carried unchanged from HEAD is legacy and is NOT judged, so the
store's pre-existing closures (many with no closure table at all) never block
an unrelated commit.

Evidence classes (every class present is validated; at least one is required)
------------------------------------------------------------------------------
  F2P       [task.closure.green] + [task.closure.red]. Structure, 40-hex revs
            that are commits and ancestors of the commit being made, runner
            allowlist, script test paths present in the index, and then a
            RE-RUN: the command must fail BEHAVIOURALLY at red.rev (the index's
            test files overlaid on that tree) and pass, with >0 tests passed,
            on the staged tree. Both trees are materialized into temp dirs from
            git objects (`read-tree` into a private index + `checkout-index`),
            never by touching the repository's index or working tree, and are
            removed on every exit path.
  doc-only  closure.doc_only_commit: an ancestor, non-root, non-merge commit
            that touches ONLY doc paths (predicate `is_doc_path`, below).
  duplicate closure.duplicate_of: an existing row (not itself) whose status is
            pending/claimed/done.
  ruling    [task.closure.ruling]: kind judgment|untestable with its rationale,
            approved_by, approved_at and approved_via = "tty". `cancelled`
            ALWAYS requires this class.

Runner allowlist (same rule as the backlog CLI): `cargo test ...`,
`pytest ...`, `python3 -m pytest ...`, `bash|sh <script under a tests/ dir>`.
The command is split on whitespace and exec'd as argv — never through a shell —
and a command containing any shell metacharacter is refused. Timeout:
BACKLOG_TEST_TIMEOUT_SECS, clamped to 1..7200, default 1800.

Exit codes: 0 nothing to judge or every judged row holds / 1 a judged row lacks
or contradicts its evidence (including an unparseable staged store) /
2 UNDETERMINED (git failure, unreadable HEAD store, test could not be run or
timed out, temp tree could not be materialized). Both 1 and 2 block in
.githooks/pre-commit. There is deliberately no bypass flag and no bypass env.
"""

from __future__ import annotations

import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
from pathlib import Path

try:
    import tomllib
except ImportError:  # pragma: no cover - python < 3.11
    tomllib = None

TAG = "closure-evidence"
STORE_FILES = (".backlog/tasks.toml", ".backlog/tasks.done.toml")
TERMINAL = ("done", "cancelled")
DUPLICATE_TARGET_STATUSES = ("pending", "claimed", "done")
RULING_KINDS = ("judgment", "untestable")
HEX40 = re.compile(r"^[0-9a-f]{40}$")
# Any of these in a recorded cmd means it was not a plain argv; refuse.
SHELL_META = set(";&|<>$`\\'\"*?()[]{}!#~\n\r\t")
EXCERPT_MAX = 4096
TIMEOUT_DEFAULT, TIMEOUT_MIN, TIMEOUT_MAX = 1800, 1, 7200
# Reason names that commit the row to a specific evidence class.
F2P_REASONS = ("fixed-f2p", "already-fixed", "obsolete", "fixed", "f2p")
# git env that would point a child process at THIS repo's index / git dir.
GIT_LOCATION_ENV = (
    "GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES", "GIT_COMMON_DIR", "GIT_PREFIX",
    "GIT_NAMESPACE",
)


class Undetermined(Exception):
    """The gate could not reach a verdict. Exit 2 (blocks)."""


# --------------------------------------------------------------------- doc path
def is_doc_path(path: str) -> bool:
    """Doc-only predicate. MUST stay identical to the backlog CLI's
    `--doc-only` predicate (crates/backlog, spec close-evidence 2026-10-01):
    `*.md`, `docs/**`, `README*` are docs, EXCEPT prompt-bearing markdown,
    which is code: any `SKILL.md`, and any `.md` under a directory named
    `agents`, `commands` or `skills`. Comments/docstrings inside code files are
    code (a `.rs`/`.py` file is never a doc path)."""
    parts = path.split("/")
    base = parts[-1]
    lower = base.lower()
    if lower == "skill.md":
        return False
    if lower.endswith(".md") and any(d in ("agents", "commands", "skills") for d in parts[:-1]):
        return False
    if lower.endswith(".md"):
        return True
    if parts[0] == "docs" and len(parts) > 1:
        return True
    return base.startswith("README")


# --------------------------------------------------------------------- git
def _git(args: list[str], *, env: dict | None = None, text: bool = True) -> subprocess.CompletedProcess:
    try:
        return subprocess.run(["git", *args], capture_output=True, text=text, env=env)
    except OSError as e:
        raise Undetermined(f"cannot run git: {e}") from e


def _git_ok(args: list[str], what: str, **kw) -> str:
    p = _git(args, **kw)
    if p.returncode != 0:
        err = p.stderr if isinstance(p.stderr, str) else p.stderr.decode("utf-8", "replace")
        raise Undetermined(f"{what}: git {' '.join(args)} exited {p.returncode}: {err.strip()}")
    return p.stdout


def head_rev() -> str | None:
    p = _git(["rev-parse", "--verify", "-q", "HEAD^{commit}"])
    if p.returncode == 0:
        return p.stdout.strip()
    # Unborn branch: every staged terminal row is new — the strictest reading.
    sym = _git(["symbolic-ref", "-q", "HEAD"])
    if sym.returncode == 0:
        return None
    raise Undetermined(f"HEAD does not resolve and is not an unborn branch: {p.stderr.strip()}")


def parent_revs(head: str | None) -> list[str]:
    """The parents of the commit being made: HEAD plus any MERGE_HEAD entries."""
    revs = [head] if head else []
    mh_path = _git_ok(["rev-parse", "--git-path", "MERGE_HEAD"], "locate MERGE_HEAD").strip()
    try:
        with open(mh_path, encoding="utf-8") as fh:
            revs += [ln.strip() for ln in fh if ln.strip()]
    except FileNotFoundError:
        pass
    except OSError as e:
        raise Undetermined(f"cannot read MERGE_HEAD ({mh_path}): {e}") from e
    return revs


def store_changed(head: str | None) -> bool:
    if head is None:
        out = _git_ok(["ls-files", "-z", "--", *STORE_FILES], "list staged store files")
        return bool(out.strip("\0"))
    p = _git(["diff", "--cached", "--quiet", head, "--", *STORE_FILES])
    if p.returncode == 0:
        return False
    if p.returncode == 1:
        return True
    raise Undetermined(f"git diff --cached exited {p.returncode}: {p.stderr.strip()}")


def blob_text(treeish: str | None, path: str) -> str | None:
    """Content of `path` at `treeish` (None = the index); None when absent."""
    if treeish is None:
        ls = _git_ok(["ls-files", "-z", "--full-name", "--", f":(literal){path}"], f"ls-files {path}")
        if path not in ls.split("\0"):
            return None
        spec = f":{path}"
    else:
        ls = _git_ok(["ls-tree", "-z", "--name-only", treeish, "--", path], f"ls-tree {path}")
        if path not in ls.split("\0"):
            return None
        spec = f"{treeish}:{path}"
    raw = _git_ok(["show", spec], f"read {spec}", text=False)
    try:
        return raw.decode("utf-8")
    except UnicodeDecodeError as e:
        raise ParseFailure(f"{spec} is not UTF-8: {e}") from e


class ParseFailure(Exception):
    pass


def load_rows(treeish: str | None) -> list[dict]:
    rows: list[dict] = []
    for f in STORE_FILES:
        text = blob_text(treeish, f)
        if text is None:
            continue
        try:
            doc = tomllib.loads(text)
        except tomllib.TOMLDecodeError as e:
            side = "staged (index)" if treeish is None else treeish[:12]
            raise ParseFailure(f"{f} at {side} is not valid TOML: {e}") from e
        tasks = doc.get("task", [])
        if not isinstance(tasks, list) or not all(isinstance(t, dict) for t in tasks):
            raise ParseFailure(f"{f}: `task` is not an array of tables")
        rows += tasks
    return rows


def in_index(path: str) -> bool:
    out = _git_ok(["ls-files", "-z", "--full-name", "--", f":(literal){path}"], f"ls-files {path}")
    return path in out.split("\0")


def is_commit(rev: str) -> bool:
    return _git(["cat-file", "-e", f"{rev}^{{commit}}"]).returncode == 0


def is_ancestor_of_any(rev: str, parents: list[str]) -> bool:
    for p in parents:
        r = _git(["merge-base", "--is-ancestor", rev, p])
        if r.returncode == 0:
            return True
        if r.returncode != 1:
            raise Undetermined(f"git merge-base --is-ancestor {rev} {p} exited {r.returncode}: {r.stderr.strip()}")
    return False


# --------------------------------------------------------------------- runner
def timeout_secs() -> int:
    raw = os.environ.get("BACKLOG_TEST_TIMEOUT_SECS")
    if raw is None or raw.strip() == "":
        return TIMEOUT_DEFAULT
    try:
        v = int(raw.strip())
    except ValueError:
        print(f"{TAG}: BACKLOG_TEST_TIMEOUT_SECS={raw!r} is not an integer; using {TIMEOUT_DEFAULT}s",
              file=sys.stderr)
        return TIMEOUT_DEFAULT
    return max(TIMEOUT_MIN, min(TIMEOUT_MAX, v))


def _clean_rel(p: str) -> str | None:
    if p.startswith("./"):
        p = p[2:]
    if not p or p.startswith("/") or any(seg in ("", "..") for seg in p.split("/")):
        return None
    return p


def check_cmd(runner, cmd) -> tuple[list[str], list[str], list[str]]:
    """(argv, errors, index_paths_to_require)."""
    errs: list[str] = []
    if not isinstance(cmd, str) or not cmd.strip():
        return [], ["closure.green.cmd is missing or not a non-empty string"], []
    bad = sorted({c for c in cmd if c in SHELL_META})
    if bad:
        errs.append(f"closure.green.cmd {cmd!r} contains shell metacharacter(s) {''.join(bad)!r}")
    argv = cmd.split()
    need: list[str] = []
    a0 = argv[0]
    if a0 == "cargo" and len(argv) >= 2 and argv[1] == "test":
        pass
    elif a0 == "pytest" or (a0 == "python3" and argv[1:3] == ["-m", "pytest"]):
        start = 1 if a0 == "pytest" else 3
        for a in argv[start:]:
            if a.startswith("-"):
                continue
            p = _clean_rel(a.split("::", 1)[0])
            if p is None:
                errs.append(f"closure.green.cmd: pytest path {a!r} is not a clean repo-relative path")
            elif p.endswith(".py"):
                need.append(p)
    elif a0 in ("bash", "sh"):
        if len(argv) < 2:
            errs.append(f"closure.green.cmd {cmd!r}: {a0} needs a script path")
        else:
            p = _clean_rel(argv[1])
            if p is None:
                errs.append(f"closure.green.cmd: script {argv[1]!r} is not a clean repo-relative path")
            elif "tests" not in p.split("/")[:-1]:
                errs.append(f"closure.green.cmd: script {p!r} is not under a tests/ directory")
            else:
                need.append(p)
    else:
        errs.append(
            f"closure.green.cmd {cmd!r}: runner outside the allowlist "
            "(cargo test / pytest / python3 -m pytest / bash|sh <tests/ script>)")
    if runner != a0:
        errs.append(f"closure.green.runner = {runner!r} does not match the command's program {a0!r}"
                    if isinstance(runner, str) else "closure.green.runner is missing or not a string")
    elif runner not in ("cargo", "pytest", "python3", "bash", "sh"):
        errs.append(f"closure.green.runner = {runner!r} is outside the allowlist")
    return argv, errs, need


def run_test(argv: list[str], cwd: Path, repo_root: str) -> tuple[int, str]:
    env = {k: v for k, v in os.environ.items() if k not in GIT_LOCATION_ENV}
    if argv[0] == "cargo" and "CARGO_TARGET_DIR" not in env:
        env["CARGO_TARGET_DIR"] = os.path.join(repo_root, "target")
    limit = timeout_secs()
    try:
        proc = subprocess.Popen(argv, cwd=str(cwd), env=env, stdout=subprocess.PIPE,
                                stderr=subprocess.STDOUT, start_new_session=True)
    except OSError as e:
        raise Undetermined(f"cannot start {argv[0]!r}: {e}") from e
    try:
        out, _ = proc.communicate(timeout=limit)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(proc.pid, signal.SIGKILL)
        except OSError:
            proc.kill()
        proc.communicate()
        raise Undetermined(f"`{' '.join(argv)}` did not finish within {limit}s "
                           "(BACKLOG_TEST_TIMEOUT_SECS)")
    return proc.returncode, out.decode("utf-8", "replace")


def count(pattern: str, out: str) -> int:
    return sum(int(m) for m in re.findall(pattern, out, flags=re.IGNORECASE))


# A RED that fails at BUILD / collection time (compile error, test API absent)
# is not a behavioural RED (spec). Only knowable for runners with a known
# output format; a bash script's failure is judged by its exit status alone.
BUILD_FAILURE_MARKERS = {
    "cargo": ("could not compile", "error[e", "error: no test target named",
              "error: package id specification", "error: could not find `cargo.toml`"),
    "pytest": ("modulenotfounderror", "importerror", "errors during collection",
               "error collecting"),
}


def behavioural_red(argv: list[str], rc: int, out: str) -> str | None:
    """None when (rc, out) is a behavioural test failure; else why it is not."""
    if rc == 0:
        return "the test PASSED at red.rev (exit 0) — nothing was reproduced"
    if rc < 0:
        return f"the test was killed by signal {-rc}, not a test failure"
    if rc in (126, 127):
        return f"exit {rc} means the program could not be run, not a test failure"
    low = out.lower()
    family = "cargo" if argv[0] == "cargo" else ("pytest" if argv[0] in ("pytest", "python3") else "")
    hit = next((m for m in BUILD_FAILURE_MARKERS.get(family, ()) if m in low), None)
    if hit:
        return f"the failure is a build/collection failure ({hit!r} in output), not a behavioural RED"
    if argv[0] in ("cargo", "pytest", "python3"):
        if count(r"(\d+) failed", out) < 1:
            return "no failing test was reported (0 failed) — the failure is not behavioural"
        if argv[0] != "cargo" and rc != 1:
            return f"pytest exit {rc} is an error/usage exit, not a test failure (exit 1)"
    return None


# --------------------------------------------------------------------- trees
def materialize(dest: Path, tree: str | None, overlay: list[str]) -> None:
    """Write `tree` (None = the index) into dest; overlay paths from the index."""
    dest.mkdir(parents=True, exist_ok=True)
    prefix = str(dest) + "/"
    if tree is None:
        _git_ok(["checkout-index", "-a", "-f", f"--prefix={prefix}"], "materialize the staged tree")
        return
    idx = str(dest.parent / (dest.name + ".index"))
    env = dict(os.environ)
    env["GIT_INDEX_FILE"] = idx
    try:
        _git_ok(["read-tree", tree], f"read-tree {tree}", env=env)
        _git_ok(["checkout-index", "-a", "-f", f"--prefix={prefix}"], f"materialize {tree}", env=env)
    finally:
        try:
            os.unlink(idx)
        except FileNotFoundError:
            pass
    if overlay:
        _git_ok(["checkout-index", "-f", f"--prefix={prefix}", "--", *overlay],
                "overlay staged test files onto the red tree")


def is_test_path(p: str) -> bool:
    parts = p.split("/")
    base = parts[-1]
    return (any(d in ("tests", "test") for d in parts[:-1])
            or (base.startswith("test_") and base.endswith(".py"))
            or base.endswith("_test.py") or base == "conftest.py")


def staged_test_paths() -> list[str]:
    """Test files the red tree gets from the index (the test is the one being
    committed; the code is the red rev's). Mirrors the backlog CLI's overlay of
    test files onto the detached red worktree."""
    out = _git_ok(["ls-files", "-z", "--full-name"], "list index")
    return [p for p in out.split("\0") if p and is_test_path(p)]


# --------------------------------------------------------------------- judging
def closure_key(row: dict):
    return (row.get("status"), repr(row.get("closure")))


def select_judged(head_rows: list[dict], idx_rows: list[dict]) -> tuple[list[dict], list[str]]:
    errs: list[str] = []
    by_id: dict[str, list[dict]] = {}
    for r in head_rows:
        rid = r.get("id")
        if isinstance(rid, str):
            by_id.setdefault(rid, []).append(r)
    judged = []
    for r in idx_rows:
        if r.get("status") not in TERMINAL:
            continue
        rid = r.get("id")
        if not isinstance(rid, str) or not rid:
            errs.append(f"a staged {r.get('status')!r} row has no string `id`; its closure cannot be attributed")
            continue
        olds = by_id.get(rid, [])
        if any(o.get("status") in TERMINAL and closure_key(o) == closure_key(r) for o in olds):
            continue  # legacy / unchanged terminal row
        judged.append(r)
    return judged, errs


class Ctx:
    def __init__(self, parents, all_rows, repo_root, tmp):
        self.parents = parents
        self.all_rows = all_rows
        self.repo_root = repo_root
        self.tmp = tmp
        self.n = 0

    def newdir(self, label: str) -> Path:
        self.n += 1
        return Path(self.tmp) / f"{self.n:03d}-{label}"


def _nonempty_str(v) -> bool:
    return isinstance(v, str) and v.strip() != ""


def check_rev(field: str, rev, parents: list[str]) -> list[str]:
    if not isinstance(rev, str) or not HEX40.match(rev):
        return [f"{field} = {rev!r} is not a 40-hex commit id"]
    if not is_commit(rev):
        return [f"{field} = {rev} is not a commit in this repository"]
    if not parents:
        return [f"{field} = {rev} cannot be an ancestor: the commit being made has no parent"]
    if not is_ancestor_of_any(rev, parents):
        return [f"{field} = {rev} is not an ancestor of the commit being made (HEAD / MERGE_HEAD)"]
    return []


def check_f2p(row: dict, cl: dict, ctx: Ctx) -> list[str]:
    errs: list[str] = []
    green, red = cl.get("green"), cl.get("red")
    if not isinstance(green, dict):
        errs.append("closure.green is missing (an F2P closure needs the observed GREEN)")
    if not isinstance(red, dict):
        errs.append("closure.red is missing (an F2P closure needs the observed behavioural RED)")
    if errs:
        return errs
    # ---- green shape
    g_exit = green.get("exit")
    if not isinstance(g_exit, int) or isinstance(g_exit, bool) or g_exit != 0:
        errs.append(f"closure.green.exit = {g_exit!r} (a green observation must exit 0)")
    g_passed = green.get("passed")
    if not isinstance(g_passed, int) or isinstance(g_passed, bool) or g_passed < 1:
        errs.append(f"closure.green.passed = {g_passed!r} (0 tests passed is not a green)")
    errs += check_rev("closure.green.rev", green.get("rev"), ctx.parents)
    oa = green.get("observed_at")
    if not isinstance(oa, int) or isinstance(oa, bool) or oa <= 0:
        errs.append(f"closure.green.observed_at = {oa!r} is not a positive integer timestamp")
    if not _nonempty_str(green.get("output_digest")):
        errs.append("closure.green.output_digest is missing or empty")
    ex = green.get("excerpt")
    if not isinstance(ex, str):
        errs.append("closure.green.excerpt is missing or not a string")
    elif len(ex.encode("utf-8")) > EXCERPT_MAX:
        errs.append(f"closure.green.excerpt exceeds {EXCERPT_MAX} bytes")
    argv, cmd_errs, need = check_cmd(green.get("runner"), green.get("cmd"))
    errs += cmd_errs
    for p in need:
        if not in_index(p):
            errs.append(f"closure.green.cmd test path {p!r} is not in the index (an uncommitted test is not evidence)")
    # ---- red shape
    r_exit = red.get("exit")
    if not isinstance(r_exit, int) or isinstance(r_exit, bool) or r_exit == 0:
        errs.append(f"closure.red.exit = {r_exit!r} (a RED observation must exit non-zero)")
    if red.get("kind") != "behavioural":
        errs.append(f"closure.red.kind = {red.get('kind')!r} (only a \"behavioural\" RED is accepted)")
    errs += check_rev("closure.red.rev", red.get("rev"), ctx.parents)
    if errs:
        return errs  # do not spend a test run on a closure that is already refused
    # ---- re-run: red at red.rev (+ staged tests), green on the staged tree
    red_dir = ctx.newdir("red")
    materialize(red_dir, red["rev"], staged_test_paths())
    rc, out = run_test(argv, red_dir, ctx.repo_root)
    why = behavioural_red(argv, rc, out)
    if why:
        errs.append(f"closure.red re-run at {red['rev'][:12]}: {why}. Output tail:\n{_tail(out)}")
    green_dir = ctx.newdir("green")
    materialize(green_dir, None, [])
    rc, out = run_test(argv, green_dir, ctx.repo_root)
    passed = count(r"(\d+) passed", out)
    if rc != 0:
        errs.append(f"closure.green re-run on the staged tree exited {rc} (recorded green does not hold). "
                    f"Output tail:\n{_tail(out)}")
    elif passed < 1:
        errs.append("closure.green re-run on the staged tree reported 0 passed (a run that tests nothing "
                    f"is not a green). Output tail:\n{_tail(out)}")
    return errs


def _tail(out: str, n: int = 8) -> str:
    return "\n".join("      | " + ln for ln in out.rstrip().splitlines()[-n:])


def check_doc_only(cl: dict, ctx: Ctx) -> list[str]:
    c = cl.get("doc_only_commit")
    errs = check_rev("closure.doc_only_commit", c, ctx.parents)
    if errs:
        return errs
    parents = _git_ok(["rev-list", "--parents", "-n", "1", c], f"parents of {c}").split()[1:]
    if len(parents) == 0:
        return [f"closure.doc_only_commit = {c} is a root commit (no diff to judge)"]
    if len(parents) > 1:
        return [f"closure.doc_only_commit = {c} is a merge commit (its diff is not a single change)"]
    out = _git_ok(["diff-tree", "-r", "-z", "--no-renames", "--name-only", "--no-commit-id",
                   parents[0], c], f"diff-tree {c}")
    paths = [p for p in out.split("\0") if p]
    if not paths:
        return [f"closure.doc_only_commit = {c} touches no path"]
    code = [p for p in paths if not is_doc_path(p)]
    if code:
        return [f"closure.doc_only_commit = {c} touches non-doc path(s) {code} "
                "(SKILL.md and .md under agents/ commands/ skills/ are prompt-bearing = code)"]
    return []


def check_duplicate(row: dict, cl: dict, ctx: Ctx) -> list[str]:
    target = cl.get("duplicate_of")
    if not _nonempty_str(target):
        return [f"closure.duplicate_of = {target!r} is not a task id"]
    if target == row.get("id"):
        return [f"closure.duplicate_of = {target!r} names the row itself"]
    hits = [r for r in ctx.all_rows if r.get("id") == target]
    if not hits:
        return [f"closure.duplicate_of = {target!r}: no such task in the staged store"]
    if not any(r.get("status") in DUPLICATE_TARGET_STATUSES for r in hits):
        sts = sorted({str(r.get("status")) for r in hits})
        return [f"closure.duplicate_of = {target!r} has status {sts}; the target must be pending/claimed/done"]
    return []


def check_ruling(row: dict, cl: dict) -> list[str]:
    rl = cl.get("ruling")
    if not isinstance(rl, dict):
        return ["closure.ruling is missing (a cancelled or ruling-closed row needs a human TTY ruling)"]
    errs = []
    kind = rl.get("kind")
    if kind not in RULING_KINDS:
        errs.append(f"closure.ruling.kind = {kind!r} (expected one of {list(RULING_KINDS)})")
    elif kind == "judgment" and not _nonempty_str(rl.get("rationale")):
        errs.append("closure.ruling.rationale is missing for a judgment ruling")
    elif kind == "untestable" and not (_nonempty_str(rl.get("untestable_reason"))
                                       or _nonempty_str(row.get("untestable_reason"))
                                       or _nonempty_str(rl.get("rationale"))):
        errs.append("closure.ruling: an untestable ruling carries no untestable_reason/rationale")
    if not _nonempty_str(rl.get("approved_by")):
        errs.append("closure.ruling.approved_by is missing (no human approval recorded)")
    at = rl.get("approved_at")
    if not ((isinstance(at, int) and not isinstance(at, bool) and at > 0) or _nonempty_str(at)):
        errs.append("closure.ruling.approved_at is missing (no approval time recorded)")
    via = rl.get("approved_via")
    if via != "tty":
        errs.append(f"closure.ruling.approved_via = {via!r} (only an interactive \"tty\" approval closes a ruling)")
    return errs


def judge_row(row: dict, ctx: Ctx) -> list[str]:
    status = row.get("status")
    cl = row.get("closure")
    if cl is None:
        return [f"status = {status!r} with no [task.closure] table (a terminal row needs recorded evidence)"]
    if not isinstance(cl, dict):
        return ["closure is not a table"]
    errs = []
    reason = cl.get("reason")
    if not _nonempty_str(reason):
        errs.append(f"closure.reason = {reason!r} is missing or empty")
        reason = ""
    has_f2p = "green" in cl or "red" in cl or reason in F2P_REASONS
    has_doc = "doc_only_commit" in cl or "doc" in reason
    has_dup = "duplicate_of" in cl or reason == "duplicate"
    has_ruling = "ruling" in cl or reason in ("ruling", "ruling-approved", "judgment", "untestable")
    if status == "cancelled" and "ruling" not in cl:
        errs.append("status = \"cancelled\" requires closure.ruling (a human TTY ruling), which is absent")
        has_ruling = False  # already reported; nothing further to validate
    elif not (has_f2p or has_doc or has_dup or has_ruling):
        errs.append(f"closure.reason = {reason!r} with no evidence "
                    "(no green/red, doc_only_commit, duplicate_of or ruling)")
    if has_ruling:
        errs += check_ruling(row, cl)
    if has_dup:
        errs += check_duplicate(row, cl, ctx)
    if has_doc:
        errs += check_doc_only(cl, ctx)
    if has_f2p:
        errs += check_f2p(row, cl, ctx)
    return errs


def evaluate() -> tuple[int, list[str]]:
    if tomllib is None:
        raise Undetermined("python3 has no tomllib (needs >= 3.11); cannot parse the backlog store")
    repo_root = _git_ok(["rev-parse", "--show-toplevel"], "locate the work tree").strip()
    os.chdir(repo_root)
    head = head_rev()
    if not store_changed(head):
        return 0, []
    try:
        idx_rows = load_rows(None)
    except ParseFailure as e:
        return 1, [f"staged backlog store cannot be parsed: {e}"]
    try:
        head_rows = load_rows(head) if head else []
    except ParseFailure as e:
        raise Undetermined(f"HEAD's backlog store cannot be parsed, so a transition cannot be judged: {e}")
    judged, errs = select_judged(head_rows, idx_rows)
    msgs = list(errs)
    if not judged:
        return (1 if msgs else 0), msgs
    parents = parent_revs(head)
    tmp = tempfile.mkdtemp(prefix="closure-evidence-")
    try:
        ctx = Ctx(parents, idx_rows, repo_root, tmp)
        for row in judged:
            for e in judge_row(row, ctx):
                msgs.append(f"task {row.get('id')} ({row.get('status')}): {e}")
    finally:
        shutil.rmtree(tmp, ignore_errors=True)
        if os.path.exists(tmp):
            print(f"{TAG}: WARNING could not remove temp dir {tmp}", file=sys.stderr)
    return (1 if msgs else 0), msgs


def main(argv: list[str]) -> int:
    if len(argv) > 1:
        print(f"{TAG}: takes no arguments (there is no bypass flag)", file=sys.stderr)
        return 2
    try:
        code, msgs = evaluate()
    except Undetermined as e:
        print(f"{TAG}: UNDETERMINED — {e}. A closure whose evidence could not be checked is not "
              "a checked closure; blocking (exit 2).", file=sys.stderr)
        return 2
    if code == 0:
        print(f"{TAG}: no staged terminal transition lacks closure evidence.")
        return 0
    for m in msgs:
        print(f"{TAG}: BLOCK {m}", file=sys.stderr)
    print(
        f"\n{TAG}: a backlog row became terminal (or its closure changed) without observed "
        "evidence. Close it through the backlog CLI (`backlog done --test ... --red-rev ...`, "
        "`--doc-only`, `--duplicate-of`), or request a human ruling (`backlog ruling request`). "
        "Suspicion, citations and code reading are not evidence.",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
