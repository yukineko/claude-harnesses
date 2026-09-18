#!/usr/bin/env python3
"""Gate: every call site of `harness_core::projkey::{repo_root, project_key,
main_worktree_root}` must be classified in scripts/projkey-store-classification.toml.

WHY THIS GATE EXISTS
--------------------
`harness_core::projkey::repo_root` walks up until an ancestor has a `.git`
ENTRY, and returns that ancestor. In a linked git worktree `.git` is a FILE,
so `.exists()` is true and `repo_root` returns THE WORKTREE, not the shared
repo. CLAUDE.md §8 mandates that all work happens in linked worktrees, so any
durable store keyed off `repo_root` / `project_key` partitions itself per
worktree and reads back EMPTY. An empty store is not "undetermined" at the
read site — it reads as "no claim", "no conflict", "no progress", which is a
fail-open (CLAUDE.md §3: the empty set must not be reported as clean).

The fix per call site is a judgment a human must make (shared identity via
`main_worktree_root`, or genuinely worktree-local). This gate does not make
that judgment. It makes the judgment MANDATORY AND RECORDED: the set of call
sites found in the tree must exactly match the set recorded in the table, so
a NEW, unclassified call site blocks instead of shipping unnoticed.

CLI CONTRACT
------------
  argv:   no arguments (gate mode), or `--list` (maintenance mode, see below)
  env:    PROJKEY_CLASS_ROOT   tree to scan      (default: this repo)
          PROJKEY_CLASS_TABLE  table to check    (default: scripts/projkey-store-classification.toml)
  exit 0  every discovered call site is classified and every table entry is live
  exit 1  a positive finding: unclassified call site, count drift, stale entry,
          missing/invalid `scope`, stale gitignore pattern
  exit 2  UNDETERMINED (CLAUDE.md §3 — cannot determine resolves to block):
          table absent/unreadable/unparseable, scan root absent, a source file
          that cannot be read or lexed, or ZERO call sites discovered (a
          scanner that finds nothing has not proved the tree is clean).

Every exit-2 path prints a line on stderr beginning with
`projkey-store-classification: UNDETERMINED:` followed by a distinct tag, so an
undetermined verdict is distinguishable from an absent interpreter/script (which
also exits 2 but produces no such line).

SCANNED SCOPE — STATED PLAINLY BECAUSE IT IS NOT TOTAL
------------------------------------------------------
This gate scans `<root>/crates/*/src/**/*.rs` ONLY. Call sites under
`crates/*/tests/` (integration tests), under `evals/`, or in any non-Rust
consumer are NOT scanned and therefore NOT gated. That is a real coverage gap,
recorded here rather than papered over; do not read a clean exit as "no call
site exists anywhere".

SYMBOL RESOLUTION
-----------------
A "call site" is the symbol name immediately followed by `(` AFTER comments,
string literals and char literals have been removed, and it counts only when
the name actually resolves to `harness_core::projkey`:

  * `harness_core::projkey::NAME(` / `crate::projkey::NAME(` (inside harness-core)
  * `NAME(` in a file whose `use` statements import NAME from
    `harness_core::projkey`, or from a LOCAL module that re-exports it
    (`crates/condukt/src/store.rs:9` does exactly this, and downstream files
    then call the bare name — a grep for the fully qualified path misses them)
  * `<m>::NAME(` / `crate::<m>::NAME(` where `<m>` is such a local re-export module
  * `NAME(` inside `crates/harness-core/src/projkey.rs` itself (the defining module)

Deliberately NOT counted, because they are different functions that merely
share a name:

  * `harness_core::store::project_key` — a different, alnum-only,
    non-canonicalized key scheme (see crates/harness-core/src/projkey.rs:13-14)
  * crate-private helpers that happen to be named `repo_root`
    (`crates/backlog/src/config.rs`, `crates/specguard/src/main.rs`) — a file
    that defines its own and imports nothing from projkey resolves to its own
  * `fn NAME(` definitions
"""

from __future__ import annotations

import os
import re
import sys
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:  # pragma: no cover - python < 3.11
    tomllib = None

PREFIX = "projkey-store-classification"
SYMBOLS = ("repo_root", "project_key", "main_worktree_root")
VALID_SCOPES = ("shared", "worktree-local")

EXIT_OK = 0
EXIT_VIOLATION = 1
EXIT_UNDETERMINED = 2


class Undetermined(Exception):
    """Raised for every cannot-determine condition. Resolves to exit 2."""

    def __init__(self, tag: str, detail: str) -> None:
        super().__init__(f"{tag}: {detail}")
        self.tag = tag
        self.detail = detail


def undetermined(tag: str, detail: str) -> None:
    raise Undetermined(tag, detail)


# --------------------------------------------------------------------------
# Rust lexing: strip comments / string literals / char literals.
# A call site cannot live inside a comment or a string, and this repo has
# several of both that mention these symbols verbatim. An UNTERMINATED
# construct is not "assume the rest is code" — it is undetermined.
# --------------------------------------------------------------------------


def strip_rust_noise(text: str, path_for_msg: str) -> str:
    out = []
    i = 0
    n = len(text)
    while i < n:
        c = text[i]
        # line comment
        if c == "/" and i + 1 < n and text[i + 1] == "/":
            j = text.find("\n", i)
            if j == -1:
                out.append(" " * (n - i))
                i = n
            else:
                out.append(" " * (j - i))
                i = j
            continue
        # block comment (nestable in Rust)
        if c == "/" and i + 1 < n and text[i + 1] == "*":
            depth = 1
            j = i + 2
            while j < n and depth > 0:
                if text[j] == "/" and j + 1 < n and text[j + 1] == "*":
                    depth += 1
                    j += 2
                elif text[j] == "*" and j + 1 < n and text[j + 1] == "/":
                    depth -= 1
                    j += 2
                else:
                    j += 1
            if depth > 0:
                undetermined(
                    "source-unlexable",
                    f"{path_for_msg}: unterminated block comment starting at offset {i}; "
                    "the remainder of the file cannot be classified as code or comment",
                )
            out.append(" " * (j - i))
            i = j
            continue
        # raw string: r"..."  r#"..."#  (br#"..."# too)
        m = re.match(r'(?:b?r)(#*)"', text[i : i + 40])
        if m and (i == 0 or not (text[i - 1].isalnum() or text[i - 1] == "_")):
            hashes = m.group(1)
            terminator = '"' + hashes
            j = text.find(terminator, i + m.end())
            if j == -1:
                undetermined(
                    "source-unlexable",
                    f"{path_for_msg}: unterminated raw string starting at offset {i}",
                )
            j += len(terminator)
            out.append(" " * (j - i))
            i = j
            continue
        # normal string
        if c == '"':
            j = i + 1
            while j < n:
                if text[j] == "\\":
                    j += 2
                    continue
                if text[j] == '"':
                    j += 1
                    break
                j += 1
            else:
                undetermined(
                    "source-unlexable",
                    f"{path_for_msg}: unterminated string literal starting at offset {i}",
                )
            out.append(" " * (j - i))
            i = j
            continue
        # char literal — must NOT eat a lifetime (`'a`, `'static`). Only a
        # complete char literal is consumed; anything else is ordinary text.
        if c == "'":
            m = re.match(r"'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F]+\}|.)|[^\\'])'", text[i:])
            if m:
                out.append(" " * m.end())
                i += m.end()
                continue
        out.append(c)
        i += 1
    return "".join(out)


# --------------------------------------------------------------------------
# Discovery
# --------------------------------------------------------------------------

USE_RE = re.compile(r"(?:^|[{;}\s])(?:pub(?:\s*\([^)]*\))?\s+)?use\s+([^;]+);", re.S)


def _expand_use(spec: str) -> list[str]:
    """Expand one `use` body into `path::name` leaves. Group braces only."""
    spec = " ".join(spec.split())
    if "{" in spec:
        head, rest = spec.split("{", 1)
        body = rest.rsplit("}", 1)[0]
        leaves = []
        depth = 0
        cur = ""
        for ch in body:
            if ch == "{":
                depth += 1
            elif ch == "}":
                depth -= 1
            if ch == "," and depth == 0:
                leaves.append(cur)
                cur = ""
            else:
                cur += ch
        leaves.append(cur)
        out = []
        for leaf in leaves:
            leaf = leaf.strip()
            if not leaf:
                continue
            out.extend(_expand_use(head.strip() + leaf))
        return out
    return [spec.strip()]


def _leaf_name(path: str) -> tuple[str, str]:
    """('a::b::c as d') -> ('a::b::c', 'd')."""
    parts = path.split(" as ")
    full = " ".join(parts[0].split())
    alias = parts[1].strip() if len(parts) > 1 else full.split("::")[-1].strip()
    return full.replace(" ", ""), alias


def crate_src_files(root: Path) -> list[Path]:
    crates = root / "crates"
    if not crates.is_dir():
        undetermined(
            "crates-dir-missing",
            f"{crates} is not a directory; the tree to scan cannot be enumerated",
        )
    files = []
    for crate in sorted(p for p in crates.iterdir() if p.is_dir()):
        src = crate / "src"
        if not src.is_dir():
            continue
        files.extend(sorted(src.rglob("*.rs")))
    return files


def read_source(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8")
    except OSError as exc:
        undetermined("source-unreadable", f"{path}: {exc}")
    except UnicodeDecodeError as exc:
        undetermined("source-unreadable", f"{path}: not valid UTF-8 ({exc})")
    raise AssertionError("unreachable")


def discover(root: Path) -> dict[tuple[str, str], int]:
    """Return {(relative_file, symbol): call_count} for genuine projkey call sites."""
    files = crate_src_files(root)
    stripped: dict[Path, str] = {}
    for path in files:
        stripped[path] = strip_rust_noise(read_source(path), str(path.relative_to(root)))

    # Pass 1: local modules that re-export projkey symbols (`pub use`).
    # keyed by crate dir name -> module last-name -> set of re-exported symbols
    reexports: dict[str, dict[str, set[str]]] = {}
    for path, text in stripped.items():
        rel = path.relative_to(root)
        crate_name = rel.parts[1]
        mod_name = path.parent.name if path.name == "mod.rs" else path.stem
        for m in USE_RE.finditer(text):
            body = m.group(0)
            if "pub" not in body.split("use")[0]:
                continue
            for leaf in _expand_use(m.group(1)):
                full, alias = _leaf_name(leaf)
                if re.match(r"^(harness_core|crate)::projkey::", full):
                    sym = full.split("::")[-1]
                    if sym in SYMBOLS:
                        reexports.setdefault(crate_name, {}).setdefault(mod_name, set()).add(alias)

    counts: dict[tuple[str, str], int] = {}

    def bump(rel: str, sym: str, k: int = 1) -> None:
        if k:
            counts[(rel, sym)] = counts.get((rel, sym), 0) + k

    for path, text in stripped.items():
        rel_path = path.relative_to(root)
        rel = rel_path.as_posix()
        crate_name = rel_path.parts[1]
        local_mods = reexports.get(crate_name, {})
        in_projkey_module = rel == "crates/harness-core/src/projkey.rs"

        # Which bare names in THIS file resolve to projkey?
        bare: dict[str, str] = {}
        if in_projkey_module:
            bare = {s: s for s in SYMBOLS}
        for m in USE_RE.finditer(text):
            for leaf in _expand_use(m.group(1)):
                full, alias = _leaf_name(leaf)
                sym = full.split("::")[-1]
                if sym not in SYMBOLS:
                    continue
                if re.match(r"^(harness_core|crate)::projkey::", full):
                    bare[alias] = sym
                    continue
                mm = re.match(r"^(?:crate::|self::)?([A-Za-z_][A-Za-z0-9_]*)::" + sym + "$", full)
                if mm and sym in local_mods.get(mm.group(1), set()):
                    bare[alias] = sym
                    continue
                # Imported from somewhere else under the same name (e.g.
                # harness_core::store::project_key): the bare name in this file
                # is NOT the projkey one. Shadow any earlier binding.
                bare.pop(alias, None)

        for sym in SYMBOLS:
            # Fully qualified: harness_core::projkey::SYM( / crate::projkey::SYM(
            bump(
                rel,
                sym,
                len(
                    re.findall(
                        r"\b(?:harness_core|crate)\s*::\s*projkey\s*::\s*" + sym + r"\s*\(", text
                    )
                ),
            )
            # Qualified through a local re-export module: [crate::|self::]mod::SYM(
            for mod_name, syms in local_mods.items():
                if sym not in syms:
                    continue
                bump(
                    rel,
                    sym,
                    len(
                        re.findall(
                            r"(?<![:\w])(?:crate\s*::\s*|self\s*::\s*)?"
                            + mod_name
                            + r"\s*::\s*"
                            + sym
                            + r"\s*\(",
                            text,
                        )
                    ),
                )

        # Bare calls of names that resolve to projkey in this file.
        for alias, sym in bare.items():
            # `(?<![:\w.])` keeps `foo::NAME(` and `x.NAME(` out; those are a
            # different path or a method, resolved above or not at all.
            hits = len(re.findall(r"(?<![:\w.])" + re.escape(alias) + r"\s*\(", text))
            # `fn NAME(` is a DEFINITION, not a call. Subtracting it here (rather
            # than excluding it with a lookbehind) is what makes the defining
            # module `projkey.rs` count its own internal calls correctly: a
            # lookbehind plus this subtraction would remove the definition twice
            # and silently drop a real call site.
            defs = len(re.findall(r"\bfn\s+" + re.escape(alias) + r"\s*\(", text))
            bump(rel, sym, max(hits - defs, 0))

    return {k: v for k, v in counts.items() if v > 0}


# --------------------------------------------------------------------------
# Table
# --------------------------------------------------------------------------


def load_table(table_path: Path) -> list[dict]:
    if tomllib is None:
        undetermined(
            "no-toml-parser",
            "python >= 3.11 (tomllib) is required to parse the classification table",
        )
    if not table_path.exists():
        undetermined(
            "table-missing",
            f"{table_path} does not exist; no call site can be confirmed classified",
        )
    try:
        raw = table_path.read_bytes()
    except OSError as exc:
        undetermined("table-unreadable", f"{table_path}: {exc}")
    try:
        data = tomllib.loads(raw.decode("utf-8"))
    except (tomllib.TOMLDecodeError, UnicodeDecodeError) as exc:
        undetermined("table-unparseable", f"{table_path}: {exc}")
    entries = data.get("consumer", [])
    if not isinstance(entries, list):
        undetermined("table-unparseable", f"{table_path}: [[consumer]] is not an array of tables")
    return entries


def main(argv: list[str]) -> int:
    root = Path(os.environ.get("PROJKEY_CLASS_ROOT", str(Path(__file__).resolve().parent.parent)))
    table_path = Path(
        os.environ.get(
            "PROJKEY_CLASS_TABLE",
            str(Path(__file__).resolve().parent / "projkey-store-classification.toml"),
        )
    )
    list_mode = "--list" in argv

    if not root.is_dir():
        undetermined("root-missing", f"PROJKEY_CLASS_ROOT={root} is not a directory")

    found = discover(root)

    if list_mode:
        for (rel, sym), count in sorted(found.items()):
            print("[[consumer]]")
            print(f'file = "{rel}"')
            print(f'symbol = "{sym}"')
            print(f"count = {count}")
            print('scope = "shared"')
            print('kind = "TODO"')
            print('evidence = "TODO"')
            print()
        return EXIT_OK

    if not found:
        undetermined(
            "zero-consumers-discovered",
            f"the scan of {root}/crates/*/src found NO call site of "
            f"{'/'.join(SYMBOLS)}; an empty result is not a clean result — "
            "either the tree is not the one intended or the scanner is broken",
        )

    entries = load_table(table_path)

    violations: list[str] = []
    declared: dict[tuple[str, str], int] = {}

    for idx, entry in enumerate(entries):
        if not isinstance(entry, dict):
            violations.append(f"table entry #{idx}: not a table")
            continue
        if entry.get("language") == "gitignore":
            pattern = entry.get("pattern")
            if not isinstance(pattern, str) or not pattern.strip():
                violations.append(f"table entry #{idx}: gitignore entry has no `pattern`")
                continue
            gi = root / ".gitignore"
            try:
                lines = [ln.strip() for ln in gi.read_text(encoding="utf-8").splitlines()]
            except FileNotFoundError:
                violations.append(
                    f"STALE gitignore entry: {pattern!r} is claimed by the table but "
                    f"{gi} does not exist"
                )
                continue
            except (OSError, UnicodeDecodeError) as exc:
                undetermined("gitignore-unreadable", f"{gi}: {exc}")
            if pattern.strip() not in lines:
                violations.append(
                    f"STALE gitignore entry: pattern {pattern!r} is no longer present in {gi}"
                )
            continue

        file_ = entry.get("file")
        symbol = entry.get("symbol")
        count = entry.get("count")
        scope = entry.get("scope")
        kind = entry.get("kind")
        evidence = entry.get("evidence")
        label = f"{file_}::{symbol}" if file_ and symbol else f"table entry #{idx}"

        ok = True
        if not isinstance(file_, str) or not file_.strip():
            violations.append(f"table entry #{idx}: missing `file`")
            ok = False
        if not isinstance(symbol, str) or symbol not in SYMBOLS:
            violations.append(f"{label}: `symbol` must be one of {list(SYMBOLS)}, got {symbol!r}")
            ok = False
        if not isinstance(count, int) or isinstance(count, bool) or count < 1:
            violations.append(f"{label}: `count` must be a positive integer, got {count!r}")
            ok = False
        # CLAUDE.md §3: an absent or unrecognised scope is NOT defaulted here.
        # "Default to shared" is a rule for the HUMAN filling the table.
        if scope not in VALID_SCOPES:
            violations.append(
                f"{label}: `scope` must be one of {list(VALID_SCOPES)}, got {scope!r} "
                "(an unclassified call site is not a classified one)"
            )
            ok = False
        if not isinstance(kind, str) or not kind.strip():
            violations.append(f"{label}: missing `kind`")
            ok = False
        if not isinstance(evidence, str) or not evidence.strip():
            violations.append(f"{label}: missing `evidence`")
            ok = False
        if not ok:
            continue

        key = (file_, symbol)
        if key in declared:
            violations.append(f"{label}: duplicate table entry")
            continue
        declared[key] = count

    for key, count in sorted(declared.items()):
        actual = found.get(key, 0)
        if actual == 0:
            violations.append(
                f"STALE table entry: {key[0]}::{key[1]} is classified in the table but no "
                "such call site exists in the scanned tree"
            )
        elif actual != count:
            violations.append(
                f"COUNT DRIFT: {key[0]}::{key[1]} — table says {count}, tree has {actual}"
            )

    for key, actual in sorted(found.items()):
        if key not in declared:
            violations.append(
                f"UNCLASSIFIED call site: {key[0]}::{key[1]} ({actual} call(s)) is not in "
                f"{table_path.name}; classify it (scope = \"shared\" unless you have "
                "evidence the store is genuinely worktree-local)"
            )

    if violations:
        print(
            f"{PREFIX}: {len(violations)} finding(s) against {table_path}:",
            file=sys.stderr,
        )
        for v in violations:
            print(f"  - {v}", file=sys.stderr)
        return EXIT_VIOLATION

    print(f"{PREFIX}: OK — {len(found)} classified call site(s) in {root}")
    return EXIT_OK


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except Undetermined as exc:
        print(f"{PREFIX}: UNDETERMINED: {exc}", file=sys.stderr)
        print(
            f"{PREFIX}: cannot determine whether every projkey store consumer is "
            "classified; blocking (CLAUDE.md §3)",
            file=sys.stderr,
        )
        sys.exit(EXIT_UNDETERMINED)
