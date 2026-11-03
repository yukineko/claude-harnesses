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
  spec: RUSTFLAGS etc. change diagnostics), and `git rev-parse HEAD:<path>` for
  every input path. Input paths: the dir of every path-dependency crate
  (`source` null) in the package's transitive closure, the package included,
  computed by a BFS over `cargo metadata --format-version 1 --all-features`
  `resolve.nodes[].deps` (normal, dev and build edges, all platforms, all
  optional deps, so it over-approximates); plus `Cargo.lock`, `Cargo.toml`,
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
  package or a path dep outside the repo, any git call failing, the script root
  not being the git toplevel, dirty inputs, `rustc -vV` / `cargo -V` failing, a
  cache file that is unreadable / not JSON / of the wrong shape. Each prints one
  `check-compile-fail: cache not used for <label>: <reason>` line. A missing
  cache file, a missing entry and a key mismatch are silent.

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
FIXED_INPUTS = ("Cargo.lock", "Cargo.toml", "rust-toolchain.toml")
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
            self.packages = {p["id"]: p for p in self.metadata["packages"]}
            self.nodes = {n["id"]: n for n in self.metadata["resolve"]["nodes"]}
        except (ValueError, KeyError, TypeError) as e:
            raise CacheUnavailable(f"cargo metadata output not parseable: {e!r}") from e

    def crate_dirs(self, pkg: str) -> list[str]:
        """Repo-relative dirs of every path dep in `pkg`'s transitive closure."""
        try:
            roots = [
                pid
                for pid, p in self.packages.items()
                if p["name"] == pkg and p["source"] is None
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
                seen.add(pid)
                if self.packages[pid]["source"] is not None:
                    continue  # registry/git dep: pinned by Cargo.lock
                node = self.nodes.get(pid)
                if node is None:
                    raise CacheUnavailable(f"cargo metadata resolve lacks {pid}")
                todo.extend(d["pkg"] for d in node["deps"])
            dirs = set()
            base = self.root.resolve()
            for pid in seen:
                p = self.packages[pid]
                if p["source"] is not None:
                    continue
                d = Path(p["manifest_path"]).resolve().parent
                try:
                    rel = d.relative_to(base)
                except ValueError as e:
                    raise CacheUnavailable(f"path dep {p['name']} at {d} is outside the repo") from e
                dirs.add(rel.as_posix() or ".")
            return sorted(dirs)
        except (KeyError, TypeError) as e:
            raise CacheUnavailable(f"cargo metadata output not parseable: {e!r}") from e

    def git_part(self, pkg: str) -> list[tuple[str, str]]:
        """(path, HEAD object id) per input; raises when dirty or a git call fails."""
        if self.error is not None:
            raise CacheUnavailable(self.error)
        paths = self.crate_dirs(pkg) + list(FIXED_INPUTS)
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

    def key(self, label: str, git_part: list[tuple[str, str]]) -> str:
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
        if ctx is not None:
            try:
                if cache_error is not None:
                    raise CacheUnavailable(cache_error)
                git_before = ctx.git_part(pkg)
                key = ctx.key(label, git_before)
            except CacheUnavailable as e:
                print(f"check-compile-fail: cache not used for {label}: {e}", file=sys.stderr)
            if key is not None and entries.get(label) == key:
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
                    unchanged = ctx.git_part(pkg) == git_before
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
