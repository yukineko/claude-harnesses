"""Cross-file test-macro invocation removal for check-test-weakening.py.

A `macro_rules!` template that expands to a `#[test]` fn generates one test
per invocation. The same-file check in `check-test-weakening.py`'s `scan` only
fires when the scanned file itself holds such a template, so deleting
`foo!(case_a);` from `tests/a.rs` while `foo` is defined (`#[macro_export]`)
in `tests/support/macros.rs` used to pass with exit 0 (RED pin
scripts/test_check_test_weakening_crossfile_macro_20261107.py). Invocations
are therefore counted per macro name on the masked code of every changed
`.rs` file; for each name whose count DROPS, the base revision is searched for
its `macro_rules!` definitions, and a name defined (directly, or through a
macro it invokes) with a `#[test]` in its body is a test-generating macro: the
drop is reported as `test-removed`. Only names whose count drops are resolved,
so adding invocations or editing other lines never consults the definitions.
Any failure to list, read, tokenise or delimit a definition raises the gate's
`Undetermined` (block), never "not a test macro".

Split out of check-test-weakening.py only to keep that file under the
file-growth cap. The gate passes its own objects in as `gate` (a namespace
of its module globals, built in `scan`), so this module uses exactly the gate's `Undetermined`, `_mask_noncode`,
`TEST_ATTR_RE`, `blob_at` and `index_content` (the after side is the index,
backlog 54de84b4).
"""
from __future__ import annotations

import re
import subprocess

_INVOCATION_RE = re.compile(r"\b([A-Za-z_][A-Za-z0-9_]*)[ \t\r\n]*!(?!=)")
_MACRO_DEF_HEAD_RE = re.compile(
    r"\bmacro_rules[ \t\r\n]*![ \t\r\n]*([A-Za-z_][A-Za-z0-9_]*)"
)
_OPENERS = "{(["


def _is_rs(path) -> bool:
    return bool(path) and path.endswith(".rs")


def macro_invocation_counts(gate, src: str, path: str) -> dict:
    """`name -> count` of `name!` invocations in the masked code of `src`
    (`macro_rules!` itself excluded). A tokenisation failure propagates."""
    out: dict = {}
    if not src:
        return out
    for m in _INVOCATION_RE.finditer(gate._mask_noncode(src, path)):
        name = m.group(1)
        if name != "macro_rules":
            out[name] = out.get(name, 0) + 1
    return out


def _macro_def_bodies(gate, code: str, path: str, name: str) -> list:
    """Bodies (masked) of every `macro_rules! name <delim>...<close>` in the
    masked `code`. An unbalanced or missing delimiter is Undetermined."""
    bodies = []
    for m in _MACRO_DEF_HEAD_RE.finditer(code):
        if m.group(1) != name:
            continue
        i = m.end()
        while i < len(code) and code[i] in " \t\r\n":
            i += 1
        if i >= len(code) or code[i] not in _OPENERS:
            raise gate.Undetermined(f"{path}: macro_rules! {name} has no delimited body")
        depth = 0
        for j in range(i, len(code)):
            ch = code[j]
            if ch in "{([":
                depth += 1
            elif ch in "})]":
                depth -= 1
                if depth == 0:
                    bodies.append(code[i : j + 1])
                    break
        else:
            raise gate.Undetermined(
                f"{path}: unbalanced delimiters in macro_rules! {name}"
            )
    return bodies


class MacroResolver:
    """Resolves whether a macro name generates `#[test]` fns, from its
    `macro_rules!` definitions in tracked `.rs` files at `rev`."""

    def __init__(self, gate, repo: str, rev: str):
        self.gate, self.repo, self.rev = gate, repo, rev
        self._files = None
        self._code: dict = {}
        self._memo: dict = {}

    def _candidate_files(self) -> list:
        if self._files is not None:
            return self._files
        U = self.gate.Undetermined
        args = ["git", "-C", self.repo, "-c", "core.quotePath=false", "grep",
                "-l", "-z", "-E", "macro_rules", self.rev, "--", "*.rs"]
        try:
            proc = subprocess.run(args, capture_output=True, text=True)
        except OSError as exc:
            raise U(f"could not run git grep: {exc}") from exc
        if proc.returncode == 1 and not proc.stdout:
            self._files = []
            return self._files
        if proc.returncode != 0:
            raise U(
                f"git grep (macro_rules) exited {proc.returncode}: {proc.stderr.strip()}"
            )
        prefix = f"{self.rev}:"
        files = []
        for tok in proc.stdout.split("\0"):
            if not tok:
                continue
            if not tok.startswith(prefix):
                raise U(f"unexpected git grep output: {tok!r}")
            files.append(tok[len(prefix):])
        self._files = files
        return files

    def _bodies(self, name: str) -> list:
        head = re.compile(
            r"\bmacro_rules[ \t\r\n]*![ \t\r\n]*" + re.escape(name) + r"(?![A-Za-z0-9_])"
        )
        bodies = []
        for f in self._candidate_files():
            if f not in self._code:
                self._code[f] = self.gate.blob_at(self.repo, self.rev, f)
            raw = self._code[f]
            if not head.search(raw):
                continue
            bodies.extend(
                _macro_def_bodies(self.gate, self.gate._mask_noncode(raw, f), f, name)
            )
        return bodies

    def generates_tests(self, name: str, _stack=None) -> bool:
        if name in self._memo:
            return self._memo[name]
        stack = _stack or set()
        if name in stack:
            return False  # recursion through itself adds no new definition
        stack = stack | {name}
        result = False
        for body in self._bodies(name):
            if self.gate.TEST_ATTR_RE.search(body):
                result = True
                break
            inner = {m.group(1) for m in _INVOCATION_RE.finditer(body)}
            if any(
                n != "macro_rules" and self.generates_tests(n, stack) for n in inner
            ):
                result = True
                break
        if _stack is None:
            # Only a top-level answer is final: a nested one may have been cut
            # short by the recursion guard on the current stack.
            self._memo[name] = result
        return result


def macro_invocation_findings(gate, repo: str, merge_base: str, pairs: list) -> list:
    """`(path, KIND_TEST_REMOVED, detail)` for each changed `.rs` file whose
    invocation count of a test-generating macro dropped against the base."""
    resolver = MacroResolver(gate, repo, merge_base)
    found = []
    for old, new in sorted(set(pairs), key=lambda pr: (pr[1] or "", pr[0] or "")):
        if not (_is_rs(old) or _is_rs(new)):
            continue
        before_src = gate.blob_at(repo, merge_base, old) if _is_rs(old) else ""
        after_src = gate.index_content(repo, new) if _is_rs(new) else ""
        before = macro_invocation_counts(gate, before_src, old or "")
        after = macro_invocation_counts(gate, after_src, new or "")
        for name in sorted(before):
            b, a = before[name], after.get(name, 0)
            if a < b and resolver.generates_tests(name):
                found.append((
                    new or old,
                    gate.KIND_TEST_REMOVED,
                    f"invocations of test-generating macro {name}! {b} -> {a}",
                ))
    return found
