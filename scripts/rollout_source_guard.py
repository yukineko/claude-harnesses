#!/usr/bin/env python3
"""Refuse to deploy plugins from a source tree that is not the committed HEAD.

Observed 2026-11-14: `rollout-plugins.sh --plugin blastguard` ran while the
pre-push fail-open mutation (`check-fail-open-mutation.py`) had rewritten
`crates/blastguard/src/model.rs` in place (`Decision::Ask(_reason) =>
Decision::Allow`). The rollout copied and built that working tree, and the
deployed GATE turned every Ask into Allow. Only the post-hoc mirror check
noticed. The pre-push mutation stays where it is (user ruling), so the
DEPLOY side has to refuse.

Since the 2026-11-14 user ruling, check-fail-open-mutation.py no longer
writes the invoking tree. It mutates and tests a scratch temporary copy
outside the tree, with its own CARGO_TARGET_DIR, so a rollout of this tree
cannot build that mutant. It still announces each run through the marker
below, which this guard keeps honouring as a conservative second barrier.
The dirt checks here guard against any other modified source.

What "the source is the committed tree" means here, and how it is decided:

  snapshot  (before anything is copied)
    - HEAD resolves, and equals --expect-head when given;
    - the BUILD INPUTS (GUARDED below) have no staged, unstaged or untracked
      (non-ignored) change per `git status`;
    - no fail-open mutation is in flight: `<git-dir>/fail-open-mutation.active`
      is absent. check-fail-open-mutation.py creates it, and bumps the
      integer in `<git-dir>/fail-open-mutation.seq`, BEFORE it writes the
      mutant (into its scratch copy), and removes it only after the cleanup is
      confirmed (scratch copy gone, the tree's target file byte-identical).
    Prints a token "<head> <seq>" on stdout.

  verify    (after the copy, after the build and before the binary swap,
             and once more at the end)
    - the same three conditions again, plus HEAD and seq unchanged since the
      snapshot. A mutation that began after the snapshot changes seq even if
      it was already reverted by the time we look, so a mutate-then-revert
      entirely inside the build window is still caught.

  verify-deployed  (right after a plugin dir is copied into the cache)
    - every file HEAD tracks under the plugin's source dir is present in the
      deployed dir with HEAD's blob content. A mismatching file is rewritten
      from the HEAD blob (so the cache never keeps mutated source) and the
      run is refused.

Every "cannot tell" (git failed, unreadable marker/seq, unparseable token)
is a refusal (CLAUDE.md section 3). Refusal exit code: 9.

The marker lives in THIS worktree's git dir (`rev-parse --absolute-git-dir`),
so a rollout only ever looks at its own tree, never another worktree's.
"""
import argparse
import hashlib
import os
import subprocess
import sys

REFUSE = 9

# Build inputs: every crate (path dependencies such as harness-core are linked
# into the plugin binaries, and `cargo build --workspace` compiles them all),
# plus the workspace manifest, lockfile and cargo config.
GUARDED = ("crates", "Cargo.toml", "Cargo.lock", ".cargo")

# Mirrors copy_plugin_dir's excludes in rollout-plugins.sh: never copied, so
# never compared.
DEPLOY_EXCLUDED_TOP = ("target", ".git", ".in_use", ".claude")

MARKER = "fail-open-mutation.active"
SEQ = "fail-open-mutation.seq"


class Undetermined(Exception):
    pass


def git(repo, *args, binary=False):
    env = dict(os.environ, GIT_OPTIONAL_LOCKS="0")
    try:
        p = subprocess.run(["git", "-C", repo, *args], stdout=subprocess.PIPE,
                           stderr=subprocess.PIPE, env=env)
    except OSError as e:
        raise Undetermined(f"git {' '.join(args)}: {e}")
    if p.returncode != 0:
        err = p.stderr.decode("utf-8", "replace").strip()
        raise Undetermined(f"git {' '.join(args)} exited {p.returncode}: {err}")
    return p.stdout if binary else p.stdout.decode("utf-8", "replace")


def head_of(repo):
    h = git(repo, "rev-parse", "--verify", "HEAD^{commit}").strip()
    if not h:
        raise Undetermined("git rev-parse HEAD printed nothing")
    return h


def mutation_state(repo):
    """(active, seq) for this worktree's git dir. seq is the file's integer,
    or 'absent' when the file does not exist."""
    gd = git(repo, "rev-parse", "--absolute-git-dir").strip()
    if not gd:
        raise Undetermined("git rev-parse --absolute-git-dir printed nothing")
    marker = os.path.join(gd, MARKER)
    active = os.path.lexists(marker)
    seq_path = os.path.join(gd, SEQ)
    if not os.path.lexists(seq_path):
        seq = "absent"
    else:
        try:
            with open(seq_path, encoding="utf-8") as f:
                raw = f.read().strip()
        except OSError as e:
            raise Undetermined(f"cannot read {seq_path}: {e}")
        if not raw.isdigit():
            raise Undetermined(f"{seq_path} does not hold an integer: {raw!r}")
        seq = raw
    return active, seq, marker


def dirty_entries(repo):
    out = git(repo, "status", "--porcelain=v1", "-z", "--untracked-files=all",
              "--", *GUARDED, binary=True)
    return [e.decode("utf-8", "replace") for e in out.split(b"\0") if e]


def problems_now(repo):
    """Problems that hold regardless of a snapshot: dirt and an active marker."""
    probs = []
    dirty = dirty_entries(repo)
    if dirty:
        shown = "\n      ".join(dirty[:20])
        more = f"\n      ... and {len(dirty) - 20} more" if len(dirty) > 20 else ""
        probs.append("build inputs (" + ", ".join(GUARDED) + ") differ from HEAD "
                     "(uncommitted or untracked):\n      " + shown + more)
    active, seq, marker = mutation_state(repo)
    if active:
        probs.append(f"a fail-open mutation is in flight ({marker} exists). "
                     "check-fail-open-mutation.py builds a mutated GATE source (in a "
                     "scratch copy since the 2026-11-14 ruling); the rollout refuses "
                     "while that runs as a conservative barrier against deploying it. "
                     "If no mutation run is alive, the marker is stale: confirm the "
                     "tree is clean (git status) and remove it by hand.")
    return probs, seq


def refuse(stage, probs):
    print(f"rollout: REFUSING ({stage}) — the source is not exactly the committed tree, "
          "so nothing built from it may be deployed:", file=sys.stderr)
    for p in probs:
        print(f"  - {p}", file=sys.stderr)
    return REFUSE


def cmd_snapshot(a):
    try:
        head = head_of(a.repo)
        probs, seq = problems_now(a.repo)
    except Undetermined as e:
        return refuse("pre-copy", [f"cannot determine source state: {e}"])
    if a.expect_head and head != a.expect_head:
        probs.append(f"HEAD moved since the run started ({a.expect_head} -> {head})")
    if probs:
        return refuse("pre-copy", probs)
    print(f"{head} {seq}")
    return 0


def parse_token(tok):
    parts = (tok or "").split()
    if len(parts) != 2 or not parts[0] or not (parts[1].isdigit() or parts[1] == "absent"):
        raise Undetermined(f"unparseable source snapshot token {tok!r}")
    return parts[0], parts[1]


def cmd_verify(a):
    try:
        head0, seq0 = parse_token(a.snapshot)
        head = head_of(a.repo)
        probs, seq = problems_now(a.repo)
    except Undetermined as e:
        return refuse(a.stage, [f"cannot determine source state: {e}"])
    if head != head0:
        probs.append(f"HEAD moved during the run ({head0} -> {head})")
    if seq != seq0:
        probs.append(f"a fail-open mutation ran during the run (mutation seq {seq0} -> {seq}); "
                     "the build may have compiled the mutant even though the tree is "
                     "clean again")
    if probs:
        return refuse(a.stage, probs)
    return 0


def blob_sha(data):
    return hashlib.sha1(b"blob %d\0" % len(data) + data).hexdigest()


def cmd_verify_deployed(a):
    try:
        head0, _ = parse_token(a.snapshot)
        listing = git(a.repo, "ls-tree", "-r", "-z", "--full-tree", head0, "--",
                      a.src.rstrip("/") + "/", binary=True)
    except Undetermined as e:
        return refuse("post-copy", [f"cannot determine committed content: {e}"])
    prefix = a.src.rstrip("/") + "/"
    entries = [e for e in listing.split(b"\0") if e]
    if not entries:
        return refuse("post-copy", [f"HEAD {head0} tracks nothing under {prefix}"])
    bad = []
    for e in entries:
        meta, path = e.split(b"\t", 1)
        mode, typ, sha = meta.decode().split()
        path = path.decode("utf-8", "surrogateescape")
        if typ != "blob" or not path.startswith(prefix):
            continue
        rel = path[len(prefix):]
        if rel.split("/", 1)[0] in DEPLOY_EXCLUDED_TOP:
            continue
        dst = os.path.join(a.dst, rel)
        try:
            if mode == "120000":
                data = os.readlink(dst).encode("utf-8", "surrogateescape")
            else:
                with open(dst, "rb") as f:
                    data = f.read()
            ok = blob_sha(data) == sha
        except OSError:
            ok = False
        if not ok:
            bad.append((rel, sha, mode, dst))
    if not bad:
        return 0
    repaired, failed = [], []
    for rel, sha, mode, dst in bad:
        try:
            content = git(a.repo, "cat-file", "blob", sha, binary=True)
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            if os.path.lexists(dst) and (os.path.islink(dst) or mode == "120000"):
                os.unlink(dst)
            if mode == "120000":
                os.symlink(content.decode("utf-8", "surrogateescape"), dst)
            else:
                with open(dst, "wb") as f:
                    f.write(content)
                os.chmod(dst, 0o755 if mode == "100755" else 0o644)
            repaired.append(rel)
        except (OSError, Undetermined) as e:
            failed.append(f"{rel}: {e}")
    probs = [f"deployed {a.dst} differed from HEAD {head0} in: " + ", ".join(r for r, *_ in bad)
             + " (the source changed while it was being copied)"]
    if repaired:
        probs.append("restored from HEAD in the cache: " + ", ".join(repaired))
    if failed:
        probs.append("COULD NOT restore (the cache may hold non-HEAD content): " + "; ".join(failed))
    return refuse("post-copy", probs)


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("snapshot")
    s.add_argument("--repo", required=True)
    s.add_argument("--expect-head", default="")
    v = sub.add_parser("verify")
    v.add_argument("--repo", required=True)
    v.add_argument("--snapshot", required=True)
    v.add_argument("--stage", default="post-build")
    d = sub.add_parser("verify-deployed")
    d.add_argument("--repo", required=True)
    d.add_argument("--snapshot", required=True)
    d.add_argument("--src", required=True)
    d.add_argument("--dst", required=True)
    a = ap.parse_args(argv)
    try:
        return {"snapshot": cmd_snapshot, "verify": cmd_verify,
                "verify-deployed": cmd_verify_deployed}[a.cmd](a)
    except Exception as e:  # noqa: BLE001 - any crash is "cannot tell" -> refuse
        return refuse(a.cmd, [f"guard crashed: {e!r}"])


if __name__ == "__main__":
    sys.exit(main())
