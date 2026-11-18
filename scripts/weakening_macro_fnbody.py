"""Fn-body-only edits beside `macro_rules!` test templates (user ruling 2026-11-18).

Used by `scripts/check-test-weakening.py` (`scan`) as a third proven-safe
shape for a Rust integration-test file that exists at the base and holds a
`macro_rules!` test template (any matcher, repetitions included). Like
weakening_macro.py it only widens the proven-safe set: anything this module
cannot prove returns False and the gate stays Undetermined (exit 2).

Trigger: GH #1697 -- a `format!("/private/tmp/...")` literal inside
`Fx::new`, `impl Drop for Fx::drop` and a `#[test] fn` body replaced by a
helper call, plus an inserted `#[path = ..] mod x;` and edited doc comments.

The proven-safe shape (`fn_body_edit_is_safe`)
----------------------------------------------
Both sides are split into items on the comment/literal-MASKED text (the
gate's `_mask_noncode`: same length, same offsets, so strings and comments
never move a brace). Top-level items are split at bracket depth 0 after `;`
or a closing `}`; an item whose head (after its outer attributes) is `fn`,
`impl` or an inline `mod NAME {` and that ends exactly at the `}` closing its
first `{` is parsed, `impl` / `mod` bodies recursively. Every other item
(`macro_rules!`, invocations, structs, traits, ...) is opaque. Each parsed
fn is identified by its parents' heads, its own head (attributes and
signature, whitespace-collapsed) and an ordinal for repeated identities; its
INTERIOR is the lines strictly between the line of its body `{` and the line
of its closing `}`. A fn inside another fn body is part of the outer body.

The change is proven safe only when ALL hold:

1. every `macro_rules!` definition (wherever it is, fn bodies included) is a
   parsed definition and the ordered list of their raw texts is identical;
2. every invocation of a macro defined in the file (`name!` on the masked
   text, wherever it is) is identical, raw text, count and order;
3. no `#[test]` / `#[ignore]` / `#[should_panic]` attribute is added or
   removed (masked counts equal);
4. on a line diff, every line that is equal is also equal after masking (an
   edit cannot open a comment or literal that swallows existing code), and
   every changed hunk is one of:
   * an insertion the gate's `_inserted_block_is_safe` accepts (whole
     top-level items outside every template and fn body, following a `;` or
     `}`);
   * an insertion of only `#[path = "plain literal"] mod x;` declarations
     (`_path_mod_insert_ok`) at a top-level item boundary -- after `;`, `}`,
     an INNER attribute `#![...]` or at the start of the file, never after an
     outer attribute -- each `x` a name that occurs nowhere in the base code
     (so no existing `x::...` path is re-pointed at the new module);
   * `//!` / `///` doc-comment lines only, on both sides (inserted, deleted or
     edited), each verified to start in CODE state -- not inside a string
     literal or block comment -- by re-masking a probe copy;
   * lines all in the interior of ONE fn on each side, with the same identity
     on both sides, whose masked text is bracket-balanced on each side and
     holds no attribute (`#[` / `#!`), `macro_rules`, `macro_use` or `return`
     -- so a hunk cannot close the fn, gate a statement with
     `#[cfg(any())]`, or return before the checks;
5. no line moves between items: a removed fn-body line (non-blank, with an
   identifier or literal on it) may not reappear among the added lines of a
   DIFFERENT fn or of an inserted top-level item;
6. inside each edited fn (`#[test]` fns and helpers alike, since a helper's
   `assert!` checks every test that calls it), every assert-family invocation
   (`assert*!`, `debug_assert*!`, `panic!`, `unreachable!`, `unimplemented!`,
   `todo!`) of the base body survives with its exact raw text (multiset
   inclusion).

What this does NOT see: an edit inside a fn body that weakens what a kept
assertion checks (a value computed differently, a helper returning something
else) -- the "weakened in place" class the gate's module docstring already
lists as not detected. The ordinary count findings still run afterwards.
"""

from __future__ import annotations

import difflib
import re
from collections import Counter

_MACRO_DEF_RE = re.compile(r"macro_rules\s*!\s*([A-Za-z_][A-Za-z0-9_]*)\s*([({\[])")
_MACRO_RULES_RE = re.compile(r"\bmacro_rules\s*!")
_TRAILING_SEMI_RE = re.compile(r"[ \t\r\n]*;")
_ATTR_OPEN_RE = re.compile(r"\s*#\s*!?\s*\[")
_FN_HEAD_RE = re.compile(
    r"\A(?:pub(?:\s*\([^()]*\))?\s+)?(?:(?:const|async|unsafe|default)\s+)*"
    r"(?:extern\s+)?fn\s+[A-Za-z_][A-Za-z0-9_]*\b"
)
_IMPL_HEAD_RE = re.compile(r"\A(?:unsafe\s+)?impl\b")
_MOD_HEAD_RE = re.compile(r"\A(?:pub(?:\s*\([^()]*\))?\s+)?mod\s+[A-Za-z_][A-Za-z0-9_]*\s*\{")
_FORBIDDEN_EDIT_RE = re.compile(r"#\s*!?\s*\[|\bmacro_rules\b|\bmacro_use\b|\breturn\b")
_CHECK_MACRO_RE = re.compile(
    r"(?<![A-Za-z0-9_])((?:debug_)?assert[A-Za-z0-9_]*|panic|unreachable|unimplemented|todo)"
    r"\s*!\s*([({\[])"
)
_DOC_LINE_RE = re.compile(r"[ \t]*//[/!]")
_WORD_RE = re.compile(r"[A-Za-z0-9_]")


def _close(masked: str, open_idx: int):
    """Index of the bracket closing the one at `open_idx` (masked text), or None."""
    depth = 0
    for k in range(open_idx, len(masked)):
        c = masked[k]
        if c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
            if depth == 0:
                return k
            if depth < 0:
                return None
    return None


def _spans(masked: str, a: int, b: int):
    """`(start, end)` of each item in `[a, b)`, split at depth 0 after `;` or a
    closing `}` (plus one trailing `;`), or None when the stretch does not
    balance. Blank (masked) text between items belongs to no item."""
    out, depth, start, k = [], 0, None, a
    while k < b:
        c = masked[k]
        if start is None and not c.isspace():
            start = k
        end = None
        if c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
            if depth < 0:
                return None
            if depth == 0 and c == "}":
                end = k + 1
                semi = _TRAILING_SEMI_RE.match(masked, end)
                if semi and semi.end() <= b:
                    end = semi.end()
        elif c == ";" and depth == 0:
            end = k + 1
        if end is not None and start is not None:
            out.append((start, end))
            start = None
            k = end
            continue
        k += 1
    if depth != 0:
        return None
    if start is not None:
        out.append((start, b))
    return out


def _after_attrs(masked: str, s: int, e: int):
    """Index of the first byte after the outer attributes of item `[s, e)`."""
    k = s
    while True:
        m = _ATTR_OPEN_RE.match(masked, k, e)
        if not m:
            return e - len(masked[k:e].lstrip())
        close = _close(masked, m.end() - 1)
        if close is None or close >= e:
            return None
        k = close + 1


def _fns(masked: str, a: int, b: int, parents: tuple, out: list) -> bool:
    """Append `(identity, body_open, body_close)` for each fn in `[a, b)`,
    descending into `impl` and inline `mod` bodies. False when unbalanced."""
    spans = _spans(masked, a, b)
    if spans is None:
        return False
    for s, e in spans:
        rest = _after_attrs(masked, s, e)
        if rest is None:
            return False
        head_text = masked[rest:e]
        is_fn = _FN_HEAD_RE.match(head_text)
        is_container = _IMPL_HEAD_RE.match(head_text) or _MOD_HEAD_RE.match(head_text)
        if not (is_fn or is_container):
            continue
        brace = masked.find("{", rest, e)
        if brace == -1 or "}" in masked[rest:brace]:
            continue  # `fn f();` or a head this parser does not read: opaque
        close = _close(masked, brace)
        if close is None or masked[close + 1:e].strip(" \t\r\n;"):
            continue  # body does not end the item (const-generic braces, ...)
        head = " ".join(masked[s:brace].split())
        if is_fn:
            out.append((parents + (head,), brace, close))
        elif not _fns(masked, brace + 1, close, parents + (head,), out):
            return False
    return True


def _owners(masked: str, line_of):
    """Per-line owner `(identity, ordinal)` of every fn-body interior line, or
    None when the side does not balance."""
    found: list = []
    if not _fns(masked, 0, len(masked), (), found):
        return None
    owners: dict = {}
    seen: Counter = Counter()
    for ident, o, c in found:
        key = (ident, seen[ident])
        seen[ident] += 1
        for ln in range(line_of(o) + 1, line_of(c)):
            owners[ln] = (key, o, c)
    return owners


def _macro_defs(raw: str, masked: str):
    """Ordered raw texts of every `macro_rules!` definition and their names, or
    None when one is not a delimited definition."""
    defs, names = [], set()
    for m in _MACRO_DEF_RE.finditer(masked):
        close = _close(masked, m.end() - 1)
        if close is None:
            return None
        defs.append(raw[m.start():close + 1])
        names.add(m.group(1))
    if len(_MACRO_RULES_RE.findall(masked)) != len(defs):
        return None
    return defs, names


def _invocations(raw: str, masked: str, names: set):
    """Ordered raw texts of every `name!(..)` of a file-local macro, or None."""
    out = []
    for name in sorted(names):
        rx = re.compile(r"(?<![A-Za-z0-9_])" + re.escape(name) + r"\s*!(?!=)\s*([({\[])?")
        for m in rx.finditer(masked):
            if m.group(1) is None:
                return None
            close = _close(masked, m.end() - 1)
            if close is None:
                return None
            out.append((m.start(), raw[m.start():close + 1]))
    return [t for _, t in sorted(out)]


def _check_macros(raw: str, masked: str, a: int, b: int):
    """Multiset of the raw texts of assert-family invocations in `[a, b)`."""
    out: Counter = Counter()
    for m in _CHECK_MACRO_RE.finditer(masked, a, b):
        close = _close(masked, m.end() - 1)
        if close is None:
            return None
        out[raw[m.start():close + 1]] += 1
    return out


def _balanced(text: str) -> bool:
    depth = 0
    for c in text:
        depth += (c in "([{") - (c in ")]}")
        if depth < 0:
            return False
    return depth == 0


def _line_index(src: str):
    starts = [0] + [i + 1 for i, c in enumerate(src) if c == "\n"]

    def line_of(off: int) -> int:
        lo, hi = 0, len(starts) - 1
        while lo < hi:
            mid = (lo + hi + 1) // 2
            if starts[mid] <= off:
                lo = mid
            else:
                hi = mid - 1
        return lo

    return starts, line_of


_PATH_MOD_RE = re.compile(
    r"\s*#\s*\[\s*path\s*=\s*\]\s*(?:pub(?:\s*\([^()]*\))?\s+)?mod\s+([A-Za-z_][A-Za-z0-9_]*)\s*;"
)
_PATH_ATTR_RAW_RE = re.compile(r'#\s*\[\s*path\s*=\s*"[^"\\\n]*"\s*\]')
_INNER_ATTR_TAIL_RE = re.compile(r"#\s*!\s*\Z")


def _at_item_boundary(masked_prefix: str) -> bool:
    """True when `masked_prefix` ends at bracket depth 0 right after a whole
    item (`;` / `}`), an INNER attribute `#![...]`, or is blank -- never after
    an outer attribute, which the inserted item would steal from its item."""
    stack, last = [], "start"
    for k, c in enumerate(masked_prefix):
        if c in "([{":
            stack.append(k)
        elif c in ")]}":
            if not stack:
                return False
            opened = stack.pop()
            if not stack:
                inner = c == "]" and _INNER_ATTR_TAIL_RE.search(masked_prefix, 0, opened)
                last = "item" if c == "}" else ("inner" if inner else "other")
        elif not stack and c == ";":
            last = "item"
        elif not stack and not c.isspace():
            last = "other"
    return not stack and last in ("start", "item", "inner")


def _path_mod_insert_ok(raw_lines: list, masked_lines: list, j1: int, j2: int,
                        masked_before: str) -> bool:
    """True when inserted lines `[j1, j2)` are only `#[path = "plain"] mod x;`
    declarations (blank lines between allowed) at a top-level item boundary,
    each `x` a name that occurs nowhere in the base (so no existing path
    `x::...` is re-pointed at the new module)."""
    masked = "".join(masked_lines[j1:j2])
    raw = "".join(raw_lines[j1:j2])
    names, k = [], 0
    while masked[k:].strip():
        m = _PATH_MOD_RE.match(masked, k)
        if not m:
            return False
        names.append(m.group(1))
        k = m.end()
    if not names or len(_PATH_ATTR_RAW_RE.findall(raw)) != len(names):
        return False
    if not _at_item_boundary("".join(masked_lines[:j1])):
        return False
    return not any(re.search(r"(?<![A-Za-z0-9_])" + re.escape(n) + r"(?![A-Za-z0-9_])",
                             masked_before) for n in names)


def _doc_lines_in_code(src: str, starts: list, lines: list, mask) -> bool:
    """True iff each line in `lines` starts in CODE state: a probe `@` line
    inserted before each one survives masking (inside a string literal or a
    block comment it is blanked). In code state `@\\n` changes no state."""
    if not lines:
        return True
    probe, prev, marks = [], 0, []
    for ln in sorted(lines):
        probe.append(src[prev:starts[ln]])
        marks.append(sum(len(p) for p in probe))
        probe.append("@\n")
        prev = starts[ln]
    probe.append(src[prev:])
    masked = mask("".join(probe))
    return all(masked[k] == "@" for k in marks)


def fn_body_edit_is_safe(before: str, after: str, masked_before: str, masked_after: str,
                         mask, insert_ok, attr_res) -> bool:
    """True only for the proven-safe shape in the module docstring.

    `mask` is the gate's `_mask_noncode` bound to the path (raises the gate's
    Undetermined on a tokenisation failure); `insert_ok(masked_after_lines,
    j1, j2)` is the gate's `_inserted_block_is_safe`; `attr_res` are the
    `#[test]` / `#[ignore]` / `#[should_panic]` regexes."""
    if len(masked_before) != len(before) or len(masked_after) != len(after):
        return False
    db = _macro_defs(before, masked_before)
    da = _macro_defs(after, masked_after)
    if db is None or da is None or db[0] != da[0]:
        return False
    names = db[1] | da[1]
    ib = _invocations(before, masked_before, names)
    ia = _invocations(after, masked_after, names)
    if ib is None or ia is None or ib != ia:
        return False
    for rx in attr_res:
        if len(rx.findall(masked_before)) != len(rx.findall(masked_after)):
            return False
    sb, line_b = _line_index(before)
    sa, line_a = _line_index(after)
    ob = _owners(masked_before, line_b)
    oa = _owners(masked_after, line_a)
    if ob is None or oa is None:
        return False
    keys_b = {v[0] for v in ob.values()}
    keys_a = {v[0] for v in oa.values()}
    b, a = before.splitlines(keepends=True), after.splitlines(keepends=True)
    mb, ma = masked_before.splitlines(keepends=True), masked_after.splitlines(keepends=True)
    if len(mb) != len(b) or len(ma) != len(a):
        return False
    edited: dict = {}  # identity -> (before owner, after owner)
    removed, added = [], []  # (stripped line, owner identity or None)
    doc_b, doc_a = [], []
    sm = difflib.SequenceMatcher(None, b, a, autojunk=False)
    for tag, i1, i2, j1, j2 in sm.get_opcodes():
        if tag == "equal":
            if mb[i1:i2] != ma[j1:j2]:
                return False
            continue
        if tag == "insert" and (insert_ok(ma, j1, j2) or _path_mod_insert_ok(
                a, ma, j1, j2, masked_before)):
            added.extend((a[k].strip(), None) for k in range(j1, j2))
            continue
        lines_b, lines_a = range(i1, i2), range(j1, j2)
        if all(_DOC_LINE_RE.match(b[k]) and not mb[k].strip() for k in lines_b) and all(
                _DOC_LINE_RE.match(a[k]) and not ma[k].strip() for k in lines_a):
            doc_b.extend(lines_b)
            doc_a.extend(lines_a)
            continue
        own_b = {ob.get(k, (None,))[0] for k in lines_b}
        own_a = {oa.get(k, (None,))[0] for k in lines_a}
        owner = own_b | own_a
        if len(owner) != 1 or None in owner:
            return False
        (key,) = owner
        if key not in keys_b or key not in keys_a:
            return False
        text_b = "".join(mb[i1:i2])
        text_a = "".join(ma[j1:j2])
        if not (_balanced(text_b) and _balanced(text_a)):
            return False
        if _FORBIDDEN_EDIT_RE.search(text_b) or _FORBIDDEN_EDIT_RE.search(text_a):
            return False
        removed.extend((b[k].strip(), key) for k in lines_b if _WORD_RE.search(b[k]))
        added.extend((a[k].strip(), key) for k in lines_a)
        edited[key] = None
    # 5. no line moves between items
    for text, key in removed:
        if any(t == text and k != key for t, k in added):
            return False
    # 6. every assert-family invocation of an edited fn's base body survives
    for key in edited:
        _, bo, bc = next(v for v in ob.values() if v[0] == key)
        _, ao, ac = next(v for v in oa.values() if v[0] == key)
        cb = _check_macros(before, masked_before, bo, bc + 1)
        ca = _check_macros(after, masked_after, ao, ac + 1)
        if cb is None or ca is None or cb - ca:
            return False
    return _doc_lines_in_code(before, sb, doc_b, mask) and _doc_lines_in_code(
        after, sa, doc_a, mask)
