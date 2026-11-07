"""Invocation counting for `macro_rules!` test templates (backlog 4f3da889).

Used by `scripts/check-test-weakening.py` (`scan`) to decide whether a change
to a Rust integration-test file that exists at the base and holds a
`macro_rules!` test template is PROVEN not to remove or weaken a generated
test. Anything this module cannot prove returns False, and the gate stays
Undetermined (exit 2): it widens the proven-safe set, it never decides a
verdict on its own.

Pure functions over a source text and its comment/literal-masked copy (the
gate's `_mask_noncode`, same length, same offsets), so this module needs
nothing from the gate except the `#[test]` attribute regex it is handed.

The proven-safe shape (`template_invocation_edit_is_safe`)
-----------------------------------------------------------
Split each side at bracket depth 0 into an element sequence: every top-level
invocation of a TEMPLATE (a top-level `macro_rules!` whose body holds a
`#[test]`) is one element keyed by the template name only, and every
non-blank stretch of text between them (other items, the `macro_rules!`
definitions themselves, non-template invocations, attributes) is one element
keyed by its exact text. The change is proven safe only when ALL hold:

* both sides tokenise and balance;
* every `macro_rules!` is a top-level item, and every occurrence of a
  template's name followed by `!` is one of those top-level invocations,
  preceded by an item boundary (`;`, `}` or the start of the file) -- so no
  template is defined inside another macro, invoked from another macro's body
  or a fn body, reached through a path, or governed by an attribute;
* every template has exactly ONE arm, and its matcher uses only plain
  fragments (`ident`, `expr`, `literal`, `ty`, `path`, `lifetime`, `pat`,
  `pat_param`) and no repetition: an invocation's arguments then cannot select
  an arm without a `#[test]`, nor inject attributes or items into the test;
* the template set is the same on both sides;
* the element sequences differ only by INSERTED template invocations: every
  non-invocation element (so every template body, byte for byte) is unchanged
  and in place, no invocation is deleted, and each surviving invocation is
  kept in order. Its ARGUMENTS may change.

So per template the invocation count can only rise, and every generated test
still exists with the same body. What this does NOT see: an argument change
that weakens what the generated test checks (a different command string, a
different check ident, an `expr` argument that returns early). That is the
same "weakened in place" class the gate's module docstring already lists as
not detected.
"""

from __future__ import annotations

import difflib
import re

_MACRO_DEF_RE = re.compile(r"macro_rules\s*!\s*([A-Za-z_][A-Za-z0-9_]*)\s*([({\[])")
_MACRO_RULES_RE = re.compile(r"\bmacro_rules\s*!")
_INVOKE_RE = re.compile(r"([A-Za-z_][A-Za-z0-9_]*)\s*!\s*([({\[])")
_TRAILING_SEMI_RE = re.compile(r"[ \t\r\n]*;")
_FRAGMENT_RE = re.compile(r"\$\s*([A-Za-z_][A-Za-z0-9_]*)\s*:\s*([A-Za-z_][A-Za-z0-9_]*)")
_ARM_RE = re.compile(r"\s*([({\[])")
_ARROW_RE = re.compile(r"\s*=>\s*([({\[])")
_ARM_END_RE = re.compile(r"\s*;?\s*\Z")
_SAFE_FRAGMENTS = frozenset(
    {"ident", "expr", "literal", "ty", "path", "lifetime", "pat", "pat_param"}
)


def _close(masked: str, open_idx: int):
    """Index of the bracket closing the one at `open_idx` in MASKED text (no
    comments or literals left to confuse the count), or None if unbalanced."""
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


def _top_level_items(masked: str):
    """`(kind, name, start, end, at_boundary)` for every top-level
    `macro_rules! name {...}` ("def") and `name!(...)` ("inv"), in order, or
    None when the text does not balance."""
    items = []
    depth = 0
    i = 0
    n = len(masked)
    while i < n:
        c = masked[i]
        if c in "([{":
            depth += 1
            i += 1
            continue
        if c in ")]}":
            depth -= 1
            if depth < 0:
                return None
            i += 1
            continue
        word_start = (c.isalpha() or c == "_") and (
            i == 0 or not (masked[i - 1].isalnum() or masked[i - 1] == "_")
        )
        if depth == 0 and word_start:
            for kind, rx in (("def", _MACRO_DEF_RE), ("inv", _INVOKE_RE)):
                m = rx.match(masked, i)
                if not m:
                    continue
                close = _close(masked, m.end() - 1)
                if close is None:
                    return None
                end = close + 1
                semi = _TRAILING_SEMI_RE.match(masked, end)
                if semi:
                    end = semi.end()
                prev = masked[:i].rstrip()
                items.append((kind, m.group(1), i, end, not prev or prev[-1] in ";}"))
                i = end
                break
            else:
                while i < n and (masked[i].isalnum() or masked[i] == "_"):
                    i += 1
            continue
        i += 1
    if depth != 0:
        return None
    return items


def _single_plain_arm(masked_def: str) -> bool:
    """True when a masked `macro_rules! name { (matcher) => { body } }` has
    exactly one arm whose matcher holds only `_SAFE_FRAGMENTS` and no
    repetition."""
    m = _MACRO_DEF_RE.match(masked_def)
    if not m:
        return False
    outer_close = _close(masked_def, m.end() - 1)
    if outer_close is None:
        return False
    inner = masked_def[m.end():outer_close]
    a = _ARM_RE.match(inner)
    if not a:
        return False
    matcher_close = _close(inner, a.end() - 1)
    if matcher_close is None:
        return False
    matcher = inner[a.end():matcher_close]
    arrow = _ARROW_RE.match(inner, matcher_close + 1)
    if not arrow:
        return False
    body_close = _close(inner, arrow.end() - 1)
    if body_close is None or not _ARM_END_RE.match(inner, body_close + 1):
        return False
    frags = _FRAGMENT_RE.findall(matcher)
    if matcher.count("$") != len(frags):
        return False  # `$(` repetition, or a `$` this parser did not read
    return all(spec in _SAFE_FRAGMENTS for _, spec in frags)


def _elements(raw: str, masked: str, test_attr_re):
    """`(templates, elements)` for one side, or None when unprovable."""
    items = _top_level_items(masked)
    if items is None:
        return None
    defs = [it for it in items if it[0] == "def"]
    if len(_MACRO_RULES_RE.findall(masked)) != len(defs):
        return None  # a macro_rules! that is not a top-level item (nested)
    templates = {}
    for _, name, s, e, _b in defs:
        if test_attr_re.search(masked[s:e]):
            if name in templates or not _single_plain_arm(masked[s:e]):
                return None
            templates[name] = raw[s:e]
    invs = [it for it in items if it[0] == "inv" and it[1] in templates]
    for name in templates:
        occurrences = len(re.findall(r"(?<![A-Za-z0-9_])" + re.escape(name) + r"\s*!", masked))
        own = [it for it in invs if it[1] == name]
        if occurrences != len(own) or not all(it[4] for it in own):
            return None
    elements = []
    pos = 0
    for _, name, s, e, _b in invs:
        gap = raw[pos:s]
        if gap.strip():
            elements.append(("other", gap.strip()))
        elements.append(("inv", name))
        pos = e
    if raw[pos:].strip():
        elements.append(("other", raw[pos:].strip()))
    return templates, elements


def template_invocation_edit_is_safe(before: str, after: str, masked_before: str,
                                     masked_after: str, test_attr_re) -> bool:
    """True only for the proven-safe shape in the module docstring."""
    if len(masked_before) != len(before) or len(masked_after) != len(after):
        return False
    b = _elements(before, masked_before, test_attr_re)
    a = _elements(after, masked_after, test_attr_re)
    if b is None or a is None:
        return False
    (tb, eb), (ta, ea) = b, a
    if not tb or tb != ta:
        return False
    sm = difflib.SequenceMatcher(None, eb, ea, autojunk=False)
    for tag, i1, i2, j1, j2 in sm.get_opcodes():
        if tag == "equal":
            continue
        if tag == "insert" and all(el[0] == "inv" for el in ea[j1:j2]):
            continue
        return False
    for name in tb:
        if ea.count(("inv", name)) < eb.count(("inv", name)):
            return False
    return True
