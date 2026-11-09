"""Git / file-content plumbing and shared definitions for check-test-weakening.py.

Split out of `scripts/check-test-weakening.py` (2026-11-07, backlog 78817f03)
to keep the gate under the file-growth cap. Behaviour is unchanged: the gate
imports every name below into its own namespace, so `Undetermined`, the
`KIND_*` constants and these functions are the SAME objects there, and the
fail-closed contract (every git / read failure raises Undetermined -> exit 2)
is documented in the gate's module docstring.
"""

from __future__ import annotations

import os
import re
import subprocess

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


def resolve_base(repo: str, base: str) -> str:
    """Resolve the comparison base to a commit sha.

    The default is the LOCAL `main` -- the branch a worktree integrates into --
    not `origin/main`. Until 2026-10-25 the default was `origin/main` (falling
    back to `main`), and a stale `origin/main` behind local `main` charged every
    commit already on main to each commit of every branch: an unrelated commit
    printed `[ack] crates/tdd/src/git.rs: ignore-added` for a file neither it nor
    its branch touched (GitHub #1181, backlog 52d17300). User ruling 2026-10-25:
    「スキャナは現在作業しているbranch,worktreeに限定した」.

    There is NO fallback, for the default or an explicit `--base`: a base that
    does not resolve raises Undetermined (exit 2, block). Silently comparing
    against some other ref would produce a confident verdict about the wrong
    range.
    """
    out = git(repo, "rev-parse", "--verify", "--quiet", base + "^{commit}",
              allow_fail=True).strip()
    if out:
        return out
    raise Undetermined(f"base ref does not resolve: {base}")


def is_git_repo(repo: str) -> bool:
    if not os.path.isdir(repo):
        return False
    out = git(repo, "rev-parse", "--is-inside-work-tree", allow_fail=True).strip()
    return out == "true"


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


def _index_entry(repo: str, path: str):
    """`(mode, sha)` of `path`'s stage-0 entry in the INDEX git hands us, or
    None when the index has no entry for it.

    "The index" is whatever `GIT_INDEX_FILE` names, which this process
    inherits: `git commit -a` / `git commit <paths>` run the commit-msg hook
    against a temporary index, and that -- not `.git/index`, not the files on
    disk -- is what the commit will contain (backlog 54de84b4, user ruling
    2026-11-08 "Index everywhere"). An unmerged path (stages 1-3) has no
    single content to judge and is Undetermined; so is any output this parser
    does not recognise.
    """
    out = git(repo, "-c", "core.quotePath=false", "ls-files", "-s", "-z",
              "--full-name", "--", f":(literal){path}")
    entries = [e for e in out.split("\0") if e]
    if not entries:
        return None
    if len(entries) != 1:
        raise Undetermined(f"{path}: {len(entries)} index entries (unmerged?)")
    meta, sep, name = entries[0].partition("\t")
    fields = meta.split()
    if not sep or name != path or len(fields) != 3:
        raise Undetermined(f"{path}: unexpected `git ls-files -s` output: {entries[0]!r}")
    mode, sha, stage = fields
    if stage != "0":
        raise Undetermined(f"{path}: unmerged in the index (stage {stage})")
    return mode, sha


def index_content(repo: str, path: str) -> str:
    """Content of `path` in the index, or "" when the index has no entry for
    it (the change deletes it). Read with `git cat-file blob <sha>`; a read
    failure or a non-UTF-8 blob is Undetermined, never "deleted"."""
    entry = _index_entry(repo, path)
    if entry is None:
        return ""
    mode, sha = entry
    if mode not in ("100644", "100755"):
        raise Undetermined(f"{path}: in the index but not a regular file (mode {mode})")
    try:
        proc = subprocess.run(("git", "-C", repo, "cat-file", "blob", sha),
                              capture_output=True)
    except OSError as exc:
        raise Undetermined(f"could not run git cat-file: {exc}") from exc
    if proc.returncode != 0:
        raise Undetermined(
            "{}: index blob {} unreadable (git cat-file exited {}): {}".format(
                path, sha, proc.returncode, proc.stderr.decode("utf-8", "replace").strip()
            )
        )
    try:
        return proc.stdout.decode("utf-8")
    except UnicodeDecodeError as exc:
        raise Undetermined(f"{path}: index blob is not valid UTF-8: {exc}") from exc


def blob_or_none(repo: str, rev: str, path: str):
    """Like `blob_at`, but None (not "") for a path absent at `rev`, so an empty
    out-of-line body file is distinguishable from a missing one."""
    listed = git(repo, "ls-tree", "-z", "--name-only", rev, "--", path)
    if path not in listed.split("\0"):
        return None
    return blob_at(repo, rev, path)


def index_or_none(repo: str, path: str):
    """Index content of `path`, None when the index has no entry for it (a
    non-regular entry raises, via `index_content`)."""
    if _index_entry(repo, path) is None:
        return None
    return index_content(repo, path)


_PATH_ATTR_GREP = r"#[[:space:]]*\[[[:space:]]*path[[:space:]]*="


def grep_path_attr_files(repo: str, rev: str | None) -> list:
    """`.rs` files containing a `#[path` attribute, at `rev` or (None) in the
    INDEX (`git grep --cached`; backlog 54de84b4). `git grep` exits 1 for "no
    match", which is an answer; any other non-zero exit is Undetermined."""
    args = ["git", "-C", repo, "-c", "core.quotePath=false", "grep", "-l", "-z", "-E",
            _PATH_ATTR_GREP]
    if rev is not None:
        args.append(rev)
    else:
        args.insert(args.index("grep") + 1, "--cached")
    args += ["--", "*.rs"]
    try:
        proc = subprocess.run(args, capture_output=True, text=True)
    except OSError as exc:
        raise Undetermined(f"could not run git grep: {exc}") from exc
    if proc.returncode == 1 and not proc.stdout:
        return []
    if proc.returncode != 0:
        raise Undetermined(f"git grep exited {proc.returncode}: {proc.stderr.strip()}")
    prefix = f"{rev}:" if rev is not None else ""
    out = []
    for tok in proc.stdout.split("\0"):
        if not tok:
            continue
        if prefix and not tok.startswith(prefix):
            raise Undetermined(f"unexpected git grep output: {tok!r}")
        out.append(tok[len(prefix):])
    return out


# --- acknowledgements and the changed set ---------------------------------


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

    During a merge (MERGE_HEAD present) the commits being merged in are also
    read, as `base_sha..MERGE_HEAD`. The diff this gate scans runs from the
    merge-base to the INDEX, and mid-merge the index already holds the
    other side's changes, so those changes are charged to the merge commit.
    Their acknowledgments live in the other side's own commit messages, which
    `base_sha..HEAD` cannot reach (backlog 0d007aa5). The same exactness rules
    apply; an unacknowledged removal from either side still blocks.

    A MERGE_HEAD that does not resolve to a commit contributes NO extra acks:
    the restrictive reading, so a broken merge state can only cause blocks,
    never a pass. Only the first MERGE_HEAD line is honoured (`rev-parse`); the
    extra heads of an octopus merge contribute no acks, which again can only
    over-block. A MERGE_HEAD that resolves but whose log cannot be read raises
    Undetermined like every other git failure here.
    """
    body = git(repo, "log", "--format=%B", f"{base_sha}..HEAD")
    merge_head = git(repo, "rev-parse", "--verify", "--quiet", "MERGE_HEAD^{commit}",
                     allow_fail=True).strip()
    if merge_head:
        body += "\n" + git(repo, "log", "--format=%B", f"{base_sha}..{merge_head}")
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


def changed_pairs(repo: str, merge_base: str) -> list:
    """`(old_path, new_path)` for every change between `merge_base` and the INDEX.

    `--cached` makes the "after" side the index git hands this process
    (`GIT_INDEX_FILE` included), which is what the commit will contain. Until
    backlog 54de84b4 the diff ran against the working tree: an unstaged edit
    was blamed on the commit, and a weakening staged while the disk copy was
    restored passed (user ruling 2026-11-08, "Index everywhere": every mode,
    the manual CLI included, judges the index; unstaged changes are ignored).

    Two more git flags carry the fail-closed contract here:

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
        "diff", "--cached", "--name-status", "-z", "-M", merge_base,
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
