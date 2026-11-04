#!/usr/bin/env python3
"""Run the trybuild compile-fail type-contract tests and fail closed.

The `harness_core::verdict` / scope type contracts ("a fail-open is
unrepresentable") are pinned by trybuild tests that assert certain code does NOT
compile, with the exact rustc diagnostics committed as `.stderr` snapshots. A
plain `cargo check` (the pre-push type-check) never builds those fixtures, so
without this script nothing at an enforcement point ever observes the contract.
Backlog 034b6620.

Exactly these test targets are run (one `cargo test -p <crate> --test <target>`
each); every one must report `test result: ok. N passed` with N equal to the
expected number of #[test] fns:

    harness-core  verdict_compile_fail   (compile_fail tests/ui/verdict + pass verdict_pass)
    harness-core  scope_compile_fail     (pass tests/ui/scope_pass — positive control)
    blastguard    verdict_compile_fail
    donegate      verdict_compile_fail

Exit codes:
    0  every target passed (a target skipped from the pass cache counts as passed)
    1  a target failed (a fixture compiled, or a diagnostic drifted from its .stderr)
    2  UNDETERMINED — cargo/rustc missing, the active rustc is not the pinned
       rust-toolchain.toml channel, cargo exited abnormally without a test result,
       or the expected test count was not observed. Undetermined is not clean:
       it is non-zero on purpose (CLAUDE.md section 3).

`TRYBUILD` is removed from the child environment: `TRYBUILD=overwrite` makes
trybuild rewrite the snapshots and report success, which would turn this check
into a no-op. A run with `TRYBUILD` set in the environment never records into
the pass cache.

Pass cache (user ruling 2026-11-03). A target whose inputs are byte-identical to
an earlier PASS is skipped; anything that cannot be determined runs the target.
The pinned-toolchain check above always runs first, even when every target
would be skipped.

  Key: sha256 (length-prefixed fields) over the label `pkg::test`, the full
  `rustc -vV` and `cargo -V` output, this script's bytes, the values of the
  build-affecting env vars in KEY_ENV / KEY_ENV_PREFIXES (an addition to the
  spec: RUSTFLAGS etc. change diagnostics), the sorted resolved package ids of
  the package's dependency closure, and `git rev-parse HEAD:<path>` for every
  input path. The closure is a walk over `cargo metadata --format-version 1
  --all-features` `resolve.nodes[].deps` from the package, the package
  included: normal, dev and build edges, all platforms, all optional deps (so it
  over-approximates), and every package, registry and git deps included. Each
  id (`resolve.nodes[].id` = `packages[].id`) carries name, version and source
  (a git dep's commit), so a Cargo.lock change that re-resolves any closure
  member changes the key, and one that only touches packages outside the
  closure does not (spec L1-L4, user ruling 2026-11-03). Cargo.lock itself is
  NOT an input path and NOT part of the dirty check. Input paths: the dir of
  every path package (`source` null) in the closure; plus `Cargo.toml`,
  `rust-toolchain.toml`, and `.cargo` when `git ls-tree HEAD -- .cargo` lists it.
  The key is valid only when `git status --porcelain --untracked-files=all --`
  over the input paths (and `.cargo`, present at HEAD or not) prints nothing.
  The git part of the key is recomputed after a pass; if it changed while cargo
  ran, nothing is recorded.

  Skip: a valid key equal to the entry recorded for the label means cargo is
  not run; stderr gets
  `check-compile-fail: skipped <label> (inputs unchanged since a pass, key <12 hex>)`.

  Record: after a PASS the valid key is written for the label. A FAILED or
  UNDETERMINED target never writes and removes the label's entry if it had one.

  Not used (target runs): `cargo metadata` failing / unparseable / lacking the
  package, a closure member's package or resolve node missing, listed twice or
  without a readable id (only the targets whose closure needs it are affected),
  a path dep outside the repo, any git call failing, the script root
  not being the git toplevel, dirty inputs, `rustc -vV` / `cargo -V` failing, a
  cache file that is unreadable / not JSON / of the wrong shape. Each prints one
  `check-compile-fail: cache not used for <label>: <reason>` line. A missing
  cache file, a missing entry and a key mismatch are silent. An unreadable or
  malformed cache file only rules out skipping: a pass still writes its key,
  rewriting the file from scratch (or warning that it cannot, below).

  File: `$CHECK_COMPILE_FAIL_CACHE` when set and non-empty, else
  `<git rev-parse --git-common-dir>/compile-fail-cache.json` made absolute. JSON
  `{"version": 1, "entries": {"<label>": "<64 hex>"}}`, re-read immediately
  before each write and replaced via a temp file in the same dir + os.replace.
  A failed write prints `check-compile-fail: could not write cache: <err>` and
  does not change the exit code. Concurrent runs are not locked against each
  other: the last writer wins, so an entry can be lost (that target then reruns).

  Opt-out: `CHECK_COMPILE_FAIL_NO_CACHE` set to any non-empty value other than
  `0` disables both skip and record (and removal); every target runs.
"""

from __future__ import annotations

import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

try:
    import tomllib  # Python >= 3.11
except ModuleNotFoundError:  # older python3 (e.g. /usr/bin/python3 on macOS)
    tomllib = None  # type: ignore[assignment]

# (package, test target, expected number of passing #[test] fns)
TARGETS: list[tuple[str, str, int]] = [
    ("harness-core", "verdict_compile_fail", 1),
    ("harness-core", "scope_compile_fail", 1),
    ("blastguard", "verdict_compile_fail", 1),
    ("donegate", "verdict_compile_fail", 1),
]

RESULT_RE = re.compile(r"^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed", re.M)

# Workspace-level inputs hashed for every target (C1). `.cargo` is added only
# when it exists at HEAD, but it is always part of the dirtiness check.
# Cargo.lock is deliberately absent (spec L1): the resolved ids of each target's
# dependency closure are keyed instead, so a lock edit outside the closure hits.
FIXED_INPUTS = ("Cargo.toml", "rust-toolchain.toml")
CARGO_CONFIG_DIR = ".cargo"

# Env vars that change what rustc emits; their values go into the key.
KEY_ENV = (
    "RUSTFLAGS",
    "RUSTDOCFLAGS",
    "CARGO_ENCODED_RUSTFLAGS",
    "CARGO_BUILD_RUSTFLAGS",
    "CARGO_BUILD_TARGET",
    "RUSTC",
    "RUSTC_WRAPPER",
    "RUSTC_WORKSPACE_WRAPPER",
    "RUSTC_BOOTSTRAP",
    "RUSTUP_TOOLCHAIN",
)
KEY_ENV_PREFIXES = ("CARGO_PROFILE_", "CARGO_UNSTABLE_", "CARGO_TARGET_")
KEY_ENV_EXCLUDE = ("CARGO_TARGET_DIR",)

CACHE_VERSION = 1
HEX64_RE = re.compile(r"^[0-9a-f]{64}$")


def undetermined(msg: str) -> int:
    print(f"check-compile-fail: UNDETERMINED — {msg}", file=sys.stderr)
    print("check-compile-fail: blocking; 'could not run' is not 'passed'.", file=sys.stderr)
    return 2


def pinned_channel(root: Path) -> str | None:
    if tomllib is None:
        return None
    try:
        with open(root / "rust-toolchain.toml", "rb") as f:
            return str(tomllib.load(f)["toolchain"]["channel"])
    except (OSError, tomllib.TOMLDecodeError, KeyError, TypeError):
        return None


class CacheUnavailable(Exception):
    """The key or the cache cannot be determined: run the target."""


def _run(cmd: list[str], root: Path, env: dict[str, str]) -> str:
    """stdout of `cmd`; any failure (spawn or non-zero exit) is CacheUnavailable."""
    try:
        proc = subprocess.run(cmd, cwd=root, env=env, capture_output=True, text=True, check=False)
    except OSError as e:
        raise CacheUnavailable(f"could not run {' '.join(cmd[:3])}: {e}") from e
    if proc.returncode != 0:
        raise CacheUnavailable(
            f"{' '.join(cmd[:3])} exited {proc.returncode}: {proc.stderr.strip()[:200]}"
        )
    return proc.stdout


_AMBIGUOUS = object()


def _index_by_id(items: list) -> dict[str, object]:
    """Map `id` -> entry. An entry whose `id` is missing or not a non-empty
    string is left out, so only a target whose closure needs it finds it
    missing (and runs); an id listed twice maps to _AMBIGUOUS, which no lookup
    accepts. A non-list raises TypeError (whole output unparseable)."""
    if not isinstance(items, list):
        raise TypeError(f"expected a list, got {type(items).__name__}")
    out: dict[str, object] = {}
    for item in items:
        pid = item.get("id") if isinstance(item, dict) else None
        if not isinstance(pid, str) or not pid:
            continue
        out[pid] = _AMBIGUOUS if pid in out else item
    return out


class KeyContext:
    """Inputs shared by every target's key. Each part is either a value or the
    reason it could not be determined (never a default)."""

    def __init__(self, root: Path, env: dict[str, str], script: Path) -> None:
        self.root = root
        self.env = env
        self.error: str | None = None
        try:
            self._load(script)
        except CacheUnavailable as e:
            self.error = str(e)

    def _load(self, script: Path) -> None:
        top = _run(["git", "rev-parse", "--show-toplevel"], self.root, self.env).strip()
        if not top or Path(top).resolve() != self.root.resolve():
            raise CacheUnavailable(f"git toplevel {top!r} is not the script root {self.root}")
        self.rustc_vv = _run(["rustc", "-vV"], self.root, self.env)
        self.cargo_v = _run(["cargo", "-V"], self.root, self.env)
        try:
            self.script_bytes = script.read_bytes()
        except OSError as e:
            raise CacheUnavailable(f"could not read {script}: {e}") from e
        self.key_env = sorted(
            (k, v)
            for k, v in self.env.items()
            if k not in KEY_ENV_EXCLUDE and (k in KEY_ENV or k.startswith(KEY_ENV_PREFIXES))
        )
        raw = _run(
            ["cargo", "metadata", "--format-version", "1", "--all-features"], self.root, self.env
        )
        try:
            self.metadata = json.loads(raw)
            self.packages = _index_by_id(self.metadata["packages"])
            self.nodes = _index_by_id(self.metadata["resolve"]["nodes"])
        except (ValueError, KeyError, TypeError, AttributeError) as e:
            raise CacheUnavailable(f"cargo metadata output not parseable: {e!r}") from e

    def _lookup(self, table: dict[str, object], what: str, pid: str) -> dict:
        entry = table.get(pid)
        if entry is _AMBIGUOUS:
            raise CacheUnavailable(f"cargo metadata {what} lists {pid} more than once")
        if not isinstance(entry, dict):
            raise CacheUnavailable(f"cargo metadata {what} lacks {pid}")
        return entry

    def closure(self, pkg: str) -> tuple[list[str], list[str]]:
        """(repo-relative dirs of the path packages, sorted resolved package ids)
        of `pkg`'s full transitive closure, `pkg` included: every dep kind, every
        package (registry and git deps too). Any package or resolve node that
        cannot be found or read raises CacheUnavailable."""
        try:
            roots = [
                pid
                for pid, p in self.packages.items()
                if isinstance(p, dict) and p["name"] == pkg and p["source"] is None
            ]
            if len(roots) != 1:
                raise CacheUnavailable(
                    f"cargo metadata has {len(roots)} path packages named {pkg!r}"
                )
            seen: set[str] = set()
            todo = [roots[0]]
            while todo:
                pid = todo.pop()
                if pid in seen:
                    continue
                self._lookup(self.packages, "packages", pid)
                node = self._lookup(self.nodes, "resolve", pid)
                seen.add(pid)
                for d in node["deps"]:
                    dep = d["pkg"]
                    if not isinstance(dep, str) or not dep:
                        raise CacheUnavailable(f"cargo metadata resolve dep of {pid} has no id")
                    todo.append(dep)
            dirs = set()
            base = self.root.resolve()
            for pid in seen:
                p = self._lookup(self.packages, "packages", pid)
                if p["source"] is not None:
                    continue  # registry/git dep: its id carries version + source
                d = Path(p["manifest_path"]).resolve().parent
                try:
                    rel = d.relative_to(base)
                except ValueError as e:
                    raise CacheUnavailable(f"path dep {p['name']} at {d} is outside the repo") from e
                dirs.add(rel.as_posix() or ".")
            return sorted(dirs), sorted(seen)
        except (KeyError, TypeError, AttributeError) as e:
            raise CacheUnavailable(f"cargo metadata output not parseable: {e!r}") from e

    def git_part(self, dirs: list[str]) -> list[tuple[str, str]]:
        """(path, HEAD object id) per input; raises when dirty or a git call fails."""
        if self.error is not None:
            raise CacheUnavailable(self.error)
        paths = list(dirs) + list(FIXED_INPUTS)
        listed = _run(["git", "ls-tree", "HEAD", "--", CARGO_CONFIG_DIR], self.root, self.env)
        if listed.strip():
            paths.append(CARGO_CONFIG_DIR)
        status_paths = sorted(set(paths) | {CARGO_CONFIG_DIR})
        dirty = _run(
            ["git", "status", "--porcelain", "--untracked-files=all", "--", *status_paths],
            self.root,
            self.env,
        )
        if dirty.strip():
            lines = dirty.splitlines()
            shown = "; ".join(line.strip() for line in lines[:3])
            more = f" (+{len(lines) - 3} more)" if len(lines) > 3 else ""
            raise CacheUnavailable(f"inputs dirty: {shown}{more}")
        out = _run(
            ["git", "rev-parse", *(f"HEAD:{p}" for p in paths)], self.root, self.env
        ).split()
        if len(out) != len(paths):
            raise CacheUnavailable(
                f"git rev-parse returned {len(out)} ids for {len(paths)} paths"
            )
        return list(zip(paths, out))

    def key(self, label: str, git_part: list[tuple[str, str]], ids: list[str]) -> str:
        h = hashlib.sha256()

        def field(name: str, data: bytes) -> None:
            h.update(name.encode() + b"\0" + str(len(data)).encode() + b"\0" + data)

        field("label", label.encode())
        field("rustc -vV", self.rustc_vv.encode())
        field("cargo -V", self.cargo_v.encode())
        field("script", self.script_bytes)
        for k, v in self.key_env:
            field("env:" + k, v.encode())
        for path, oid in git_part:
            field("path:" + path, oid.encode())
        field("closure", str(len(ids)).encode())
        for pid in ids:
            field("id", pid.encode())
        return h.hexdigest()


def cache_path(root: Path, env: dict[str, str]) -> Path:
    override = os.environ.get("CHECK_COMPILE_FAIL_CACHE", "")
    if override:
        return Path(os.path.abspath(override))
    common = _run(["git", "rev-parse", "--git-common-dir"], root, env).strip()
    if not common:
        raise CacheUnavailable("git rev-parse --git-common-dir printed nothing")
    p = Path(common)
    if not p.is_absolute():
        p = root / p
    return p.resolve() / "compile-fail-cache.json"


def read_cache(path: Path) -> dict[str, str]:
    """Entries of the cache file; {} when it does not exist (silent).
    Anything else that is not a well-formed cache raises CacheUnavailable."""
    try:
        text = path.read_text(encoding="utf-8")
    except FileNotFoundError:
        return {}
    except (OSError, UnicodeDecodeError) as e:
        raise CacheUnavailable(f"cache file {path} unreadable: {e}") from e
    try:
        data = json.loads(text)
    except ValueError as e:
        raise CacheUnavailable(f"cache file {path} is not JSON: {e}") from e
    if (
        not isinstance(data, dict)
        or data.get("version") != CACHE_VERSION
        or not isinstance(data.get("entries"), dict)
        or not all(
            isinstance(k, str) and isinstance(v, str) and HEX64_RE.match(v)
            for k, v in data["entries"].items()
        )
    ):
        raise CacheUnavailable(f"cache file {path} has the wrong shape")
    return dict(data["entries"])


def update_cache(path: Path, label: str, key: str | None) -> None:
    """Set (key) or remove (None) the label's entry. Never raises: a failed
    write is one warning and the exit code is unaffected."""
    try:
        try:
            entries = read_cache(path)
        except CacheUnavailable:
            entries = {}  # rewrite a broken file from scratch
        if key is None:
            if label not in entries:
                return
            del entries[label]
        else:
            entries[label] = key
        payload = json.dumps({"version": CACHE_VERSION, "entries": entries}, indent=2, sort_keys=True)
        fd, tmp = tempfile.mkstemp(prefix=".compile-fail-cache.", dir=str(path.parent))
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as f:
                f.write(payload + "\n")
            os.replace(tmp, path)
        except BaseException:
            try:
                os.unlink(tmp)
            except OSError:
                pass
            raise
    except OSError as e:
        print(f"check-compile-fail: could not write cache: {e}", file=sys.stderr)


def main() -> int:
    root = Path(__file__).resolve().parent.parent
    for exe in ("cargo", "rustc"):
        if shutil.which(exe) is None:
            return undetermined(f"{exe} not found on PATH (source \"$HOME/.cargo/env\")")

    channel = pinned_channel(root)
    if channel is None:
        return undetermined(
            "rust-toolchain.toml missing, unparseable, or has no [toolchain].channel "
            "(or this python3 lacks tomllib; need >= 3.11)"
        )
    try:
        ver = subprocess.run(
            ["rustc", "--version"], cwd=root, capture_output=True, text=True, check=False
        )
    except OSError as e:
        return undetermined(f"could not run rustc: {e}")
    if ver.returncode != 0 or not ver.stdout.startswith(f"rustc {channel} "):
        return undetermined(
            f"active rustc is {ver.stdout.strip() or ver.stderr.strip()!r}, but the "
            f".stderr snapshots are pinned to {channel} (rust-toolchain.toml)"
        )

    env = {k: v for k, v in os.environ.items() if k != "TRYBUILD"}

    no_cache = os.environ.get("CHECK_COMPILE_FAIL_NO_CACHE", "") not in ("", "0")
    may_record = "TRYBUILD" not in os.environ
    ctx: KeyContext | None = None
    cpath: Path | None = None
    cache_error: str | None = None
    entries: dict[str, str] = {}
    if no_cache:
        print("check-compile-fail: pass cache disabled (CHECK_COMPILE_FAIL_NO_CACHE)", file=sys.stderr)
    else:
        if not may_record:
            print("check-compile-fail: TRYBUILD is set; passes will not be recorded", file=sys.stderr)
        ctx = KeyContext(root, env, Path(__file__).resolve())
        try:
            cpath = cache_path(root, env)
            entries = read_cache(cpath)
        except CacheUnavailable as e:
            cache_error = str(e)

    def forget(label: str) -> None:
        if cpath is not None:
            update_cache(cpath, label, None)

    failed: list[str] = []
    from_cache = 0
    for pkg, test, expected in TARGETS:
        label = f"{pkg}::{test}"
        key: str | None = None
        git_before: list[tuple[str, str]] | None = None
        dirs: list[str] = []
        if ctx is not None:
            # An unreadable cache file only rules out the skip: the key is still
            # computed so a pass rewrites the file (or warns that it cannot).
            if cache_error is not None:
                print(f"check-compile-fail: cache not used for {label}: {cache_error}", file=sys.stderr)
            try:
                if ctx.error is not None:
                    raise CacheUnavailable(ctx.error)
                dirs, ids = ctx.closure(pkg)
                git_before = ctx.git_part(dirs)
                key = ctx.key(label, git_before, ids)
            except CacheUnavailable as e:
                if cache_error is None:
                    print(f"check-compile-fail: cache not used for {label}: {e}", file=sys.stderr)
            if cache_error is None and key is not None and entries.get(label) == key:
                print(
                    f"check-compile-fail: skipped {label} "
                    f"(inputs unchanged since a pass, key {key[:12]})",
                    file=sys.stderr,
                )
                from_cache += 1
                continue

        print(f"check-compile-fail: running {label} ...", file=sys.stderr)
        try:
            proc = subprocess.run(
                ["cargo", "test", "-p", pkg, "--test", test],
                cwd=root,
                env=env,
                capture_output=True,
                text=True,
                check=False,
            )
        except OSError as e:
            forget(label)
            return undetermined(f"could not run cargo for {label}: {e}")
        out = proc.stdout + proc.stderr
        results = RESULT_RE.findall(out)
        if len(results) != 1:
            sys.stderr.write(out)
            forget(label)
            return undetermined(
                f"{label}: cargo exited {proc.returncode} with {len(results)} "
                "test-result lines (build error or target missing)"
            )
        status, passed, nfailed = results[0][0], int(results[0][1]), int(results[0][2])
        if proc.returncode == 0 and status == "ok" and passed == expected and nfailed == 0:
            print(f"check-compile-fail: ok {label} ({passed} passed)", file=sys.stderr)
            if ctx is not None and key is not None and cpath is not None and may_record:
                try:
                    unchanged = ctx.git_part(dirs) == git_before
                except CacheUnavailable as e:
                    unchanged = False
                    print(f"check-compile-fail: pass not recorded for {label}: {e}", file=sys.stderr)
                else:
                    if not unchanged:
                        print(
                            f"check-compile-fail: pass not recorded for {label}: "
                            "inputs changed while cargo ran",
                            file=sys.stderr,
                        )
                if unchanged:
                    update_cache(cpath, label, key)
            continue
        if status == "ok" and proc.returncode == 0:
            sys.stderr.write(out)
            forget(label)
            return undetermined(f"{label}: expected {expected} passing test(s), saw {passed}")
        sys.stderr.write(out)
        print(f"check-compile-fail: FAILED {label} (cargo exit {proc.returncode})", file=sys.stderr)
        forget(label)
        failed.append(label)

    if failed:
        print(
            "check-compile-fail: RED — type-contract compile-fail test(s) failed: "
            + ", ".join(failed)
            + ". A fixture that now compiles means the contract reopened; a drifted "
            "diagnostic means the .stderr must be regenerated deliberately with the "
            "pinned toolchain (TRYBUILD=overwrite) and reviewed.",
            file=sys.stderr,
        )
        return 1
    suffix = f" ({from_cache} from cache)" if from_cache > 0 else ""
    print(f"check-compile-fail: all {len(TARGETS)} trybuild targets passed{suffix}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
