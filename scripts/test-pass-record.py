#!/usr/bin/env python3
"""Tree-keyed pass record: a check that PASSED for an exact git tree is not rerun.

User ruling 2026-11-08: a test confirmed for a commit's range must not be run
again at push for that same commit. `.githooks/pre-push` asks this script
before each check it would run. Any test run may record a pass, and pre-push
records its own passes.

    python3 scripts/test-pass-record.py check  --tree <TREE> --test <ID>
    python3 scripts/test-pass-record.py record --tree <TREE> --test <ID>

The key is the exact tree object id plus the exact test id string. Only a tree
whose content is byte-identical to the one that was tested can match, so a
commit that differs from what was tested is never skipped.

check: fail-closed
  exit 0  ONLY when a valid record exists for exactly (TREE, ID).
  exit 1  for everything else, which means "run the check": no record, a file
          that is unreadable, not JSON, of the wrong shape or version, a stored
          tree or test differing from the request, a TREE that is not a
          full-length lowercase hex id, or a store that cannot be resolved.
          Every case except a plain missing record prints one stderr line.

record
  exit 0  written. The file is written to a temp file in the same dir and then
          moved into place with os.replace.
  exit 1  the write failed. Nothing usable was left behind, so the check simply
          reruns next time.
  exit 2  TREE is not a full lowercase hex id, or `git cat-file -t TREE` does
          not print `tree`. Nothing is recorded.

Store: `$TEST_PASS_RECORD_DIR` when set and non-empty. Otherwise
`<git rev-parse --git-common-dir, made absolute>/test-pass-record`. The store
is shared by this clone's worktrees. That is safe because the key is content,
not location.
File: `<store>/<sha256(ID) hex>/<TREE>.json`, containing
`{"version": 1, "tree": TREE, "test": ID}`.

Not part of the key: the toolchain version, the environment (RUSTFLAGS, ...)
and ignored files. rust-toolchain.toml is inside the tree, so the pinned
channel is covered. A local toolchain override is not.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path

VERSION = 1
HEX_RE = re.compile(r"^(?:[0-9a-f]{40}|[0-9a-f]{64})$")


class StoreUnavailable(Exception):
    pass


def store_dir() -> Path:
    override = os.environ.get("TEST_PASS_RECORD_DIR", "")
    if override:
        return Path(override)
    try:
        proc = subprocess.run(
            ["git", "rev-parse", "--git-common-dir"], capture_output=True, text=True
        )
    except OSError as e:
        raise StoreUnavailable(f"git could not be executed: {e}") from e
    out = proc.stdout.strip()
    if proc.returncode != 0 or not out:
        raise StoreUnavailable(
            f"`git rev-parse --git-common-dir` exited {proc.returncode}: "
            f"{proc.stderr.strip() or '(no output)'}"
        )
    return (Path.cwd() / out).resolve() / "test-pass-record"


def record_path(store: Path, tree: str, test: str) -> Path:
    return store / hashlib.sha256(test.encode("utf-8")).hexdigest() / f"{tree}.json"


def cmd_check(tree: str, test: str) -> int:
    if not HEX_RE.match(tree):
        print(f"test-pass-record: {tree!r} is not a full tree id — no skip", file=sys.stderr)
        return 1
    try:
        path = record_path(store_dir(), tree, test)
    except StoreUnavailable as e:
        print(f"test-pass-record: store unavailable ({e}) — no skip", file=sys.stderr)
        return 1
    try:
        raw = path.read_text(encoding="utf-8")
    except FileNotFoundError:
        return 1
    except (OSError, UnicodeDecodeError) as e:
        print(f"test-pass-record: {path} unreadable ({e}) — no skip", file=sys.stderr)
        return 1
    try:
        data = json.loads(raw)
    except ValueError as e:
        print(f"test-pass-record: {path} is not JSON ({e}) — no skip", file=sys.stderr)
        return 1
    if not isinstance(data, dict) or data.get("version") != VERSION:
        print(f"test-pass-record: {path} has the wrong shape or version — no skip", file=sys.stderr)
        return 1
    if data.get("tree") != tree or data.get("test") != test:
        print(
            f"test-pass-record: {path} records tree={data.get('tree')!r} "
            f"test={data.get('test')!r}, not the requested pair — no skip",
            file=sys.stderr,
        )
        return 1
    return 0


def cmd_record(tree: str, test: str) -> int:
    if not HEX_RE.match(tree):
        print(f"test-pass-record: {tree!r} is not a full tree id — not recorded", file=sys.stderr)
        return 2
    try:
        proc = subprocess.run(["git", "cat-file", "-t", tree], capture_output=True, text=True)
    except OSError as e:
        print(f"test-pass-record: git could not be executed ({e}) — not recorded", file=sys.stderr)
        return 2
    if proc.returncode != 0 or proc.stdout.strip() != "tree":
        print(
            f"test-pass-record: {tree} is not a tree object here "
            f"({proc.stdout.strip() or proc.stderr.strip() or 'no output'}) — not recorded",
            file=sys.stderr,
        )
        return 2
    try:
        path = record_path(store_dir(), tree, test)
        path.parent.mkdir(parents=True, exist_ok=True)
        fd, tmp = tempfile.mkstemp(prefix=".record.", dir=str(path.parent))
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as f:
                json.dump({"version": VERSION, "tree": tree, "test": test}, f)
            os.replace(tmp, path)
        except BaseException:
            try:
                os.unlink(tmp)
            except OSError:
                pass
            raise
    except (OSError, StoreUnavailable) as e:
        print(f"test-pass-record: could not write the record ({e})", file=sys.stderr)
        return 1
    return 0


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("action", choices=("check", "record"))
    ap.add_argument("--tree", required=True)
    ap.add_argument("--test", required=True)
    args = ap.parse_args(argv[1:])
    if args.action == "check":
        return cmd_check(args.tree, args.test)
    return cmd_record(args.tree, args.test)


if __name__ == "__main__":
    sys.exit(main(sys.argv))
