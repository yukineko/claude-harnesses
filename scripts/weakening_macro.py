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
`#[test]`) is one element keyed by the template name and the generated test's
name (see the subsequence rule below), and every other
top-level item between them (split on the masked text at depth 0 after `;` or
a closing `}`: the `macro_rules!` definitions themselves, other items,
non-template invocations, with any attributes they carry) is one element keyed
by its exact text. Comments between items belong to no element. The change is
proven safe only when ALL hold:

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
* the base element sequence is a SUBSEQUENCE of the new one (matched
  greedily, so the verdict does not depend on how a line diff aligns closing
  braces -- backlog ed82c49b): every non-invocation element (so every
  template body, byte for byte) is unchanged and in order, no invocation is
  deleted, and each surviving invocation keeps its place. An invocation is
  matched by template AND generated test name (the `ident` argument the
  template uses as `fn $name`, `_invocation_key`), so its OTHER arguments may
  change; when that name cannot be read off, by its exact text, so it may not
  change. Dropping one invocation and adding another of the same template
  under a new name is a removal, never an argument edit;
* every INSERTED element is either a template invocation, or a plain
  top-level helper `fn` (`_is_safe_inserted_fn`): no attribute in front, one
  balanced item, no `macro_rules`/`macro_use`/inner attribute, and a name
  that occurs nowhere in the base, so it shadows nothing an existing item
  calls. An inserted `macro_rules!` (a new or redefined template), a method
  added inside an existing `impl`, or any other inserted item is not proven.

So per template the invocation count can only rise, and every generated test
still exists with the same body. What this does NOT see: an argument change
that weakens what the generated test checks (a different command string, a
different check ident, an `expr` argument that returns early). That is the
same "weakened in place" class the gate's module docstring already lists as
not detected.
"""

from __future__ import annotations

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


_COMMA_MATCHER_RE = re.compile(
    r"\s*\$\s*\w+\s*:\s*\w+(?:\s*,\s*\$\s*\w+\s*:\s*\w+)*\s*,?\s*\Z")
_FN_META_RE = re.compile(r"\bfn\s+\$\s*([A-Za-z_][A-Za-z0-9_]*)")


def _single_plain_arm(masked_def: str):
    """For a masked `macro_rules! name { (matcher) => { body } }` with exactly
    one arm whose matcher holds only `_SAFE_FRAGMENTS` and no repetition:
    `(True, pos)`, where `pos` is the index of the comma-separated `ident`
    argument that names the generated `fn $x` -- or None when that cannot be
    read off (several `fn $..`, a matcher that is not a plain comma list).
    `(False, None)` for any other template."""
    m = _MACRO_DEF_RE.match(masked_def)
    if not m:
        return False, None
    outer_close = _close(masked_def, m.end() - 1)
    if outer_close is None:
        return False, None
    inner = masked_def[m.end():outer_close]
    a = _ARM_RE.match(inner)
    if not a:
        return False, None
    matcher_close = _close(inner, a.end() - 1)
    if matcher_close is None:
        return False, None
    matcher = inner[a.end():matcher_close]
    arrow = _ARROW_RE.match(inner, matcher_close + 1)
    if not arrow:
        return False, None
    body_close = _close(inner, arrow.end() - 1)
    if body_close is None or not _ARM_END_RE.match(inner, body_close + 1):
        return False, None
    frags = _FRAGMENT_RE.findall(matcher)
    if matcher.count("$") != len(frags):
        return False, None  # `$(` repetition, or a `$` this parser did not read
    if not all(spec in _SAFE_FRAGMENTS for _, spec in frags):
        return False, None
    metas = _FN_META_RE.findall(inner[arrow.end():body_close])
    pos = None
    if _COMMA_MATCHER_RE.match(matcher) and len(metas) == 1:
        idx = [k for k, (v, spec) in enumerate(frags) if v == metas[0] and spec == "ident"]
        pos = idx[0] if len(idx) == 1 else None
    return True, pos


def _invocation_key(raw: str, masked: str, s: int, e: int, name: str, pos):
    """Diff key of one template invocation: `("inv", template, test_name)` when
    the argument at `pos` (split at depth-0 commas of the masked argument list)
    is a bare identifier -- the name of the generated test -- so its OTHER
    arguments may change; otherwise its exact text, so it may not change at
    all. Removing one invocation and adding another of the same template under
    a different test name is therefore a removal, not an argument edit."""
    if pos is not None:
        m = _INVOKE_RE.match(masked, s)
        close = _close(masked, m.end() - 1) if m else None
        if close is not None:
            args, depth, start = [], 0, m.end()
            for k in range(m.end(), close):
                c = masked[k]
                depth += (c in "([{") - (c in ")]}")
                if c == "," and depth == 0:
                    args.append(raw[start:k].strip())
                    start = k + 1
            args.append(raw[start:close].strip())
            if pos < len(args) and re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", args[pos]):
                return ("inv", name, args[pos])
    return ("inv", name, raw[s:e].strip())


def _elements(raw: str, masked: str, test_attr_re):
    """`(templates, elements)` for one side, or None when unprovable."""
    items = _top_level_items(masked)
    if items is None:
        return None
    defs = [it for it in items if it[0] == "def"]
    if len(_MACRO_RULES_RE.findall(masked)) != len(defs):
        return None  # a macro_rules! that is not a top-level item (nested)
    templates = {}
    name_pos = {}
    for _, name, s, e, _b in defs:
        if test_attr_re.search(masked[s:e]):
            ok, name_pos[name] = _single_plain_arm(masked[s:e])
            if name in templates or not ok:
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
    for _, name, s, e, _b in invs + [(None, None, len(raw), len(raw), True)]:
        items = _items_in(raw, masked, pos, s)
        if items is None:
            return None
        elements.extend((("other", text), m) for text, m in items)
        if name is not None:
            elements.append((_invocation_key(raw, masked, s, e, name, name_pos[name]), None))
        pos = e
    return templates, elements


def _items_in(raw: str, masked: str, a: int, b: int):
    """`(raw_text, masked_text)` of each top-level item in `[a, b)`, split on
    the MASKED text at depth 0 after a `;` or a closing `}` (plus one trailing
    `;`), or None when the stretch does not balance. Text that masking blanks
    (comments) between items belongs to no item."""
    out = []
    depth = 0
    start = None
    k = a
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
            out.append((raw[start:end].strip(), masked[start:end]))
            start = None
            k = end
            continue
        k += 1
    if depth != 0:
        return None
    if start is not None:
        out.append((raw[start:b].strip(), masked[start:b]))
    return out


# An inserted helper must be exactly one plain `fn` item: no attribute in front
# (an attribute such as a constructor hook could run code before every test),
# nothing after its body.
_PLAIN_FN_RE = re.compile(
    r"\A(?:pub(?:\s*\([^()]*\))?\s+)?(?:(?:const|async|unsafe)\s+)*fn\s+"
    r"([A-Za-z_][A-Za-z0-9_]*)\b"
)
_INSERT_FORBIDDEN_RE = re.compile(r"\bmacro_rules\b|\bmacro_use\b|#[ \t\n]*!")


def _is_safe_inserted_fn(masked_item: str, masked_before: str) -> bool:
    """True when an inserted top-level item is a plain `fn` that cannot change
    an existing test: one balanced item ending at its own body's `}`, no
    `macro_rules`/`macro_use`/inner attribute in it, and a name that does not
    occur anywhere in the base -- so it shadows nothing an existing item calls
    (a local fn silently shadows a glob-imported or prelude one)."""
    m = _PLAIN_FN_RE.match(masked_item)
    if not m or not masked_item.endswith("}") or _INSERT_FORBIDDEN_RE.search(masked_item):
        return False
    depth = 0
    for k, c in enumerate(masked_item):
        if c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
            if depth < 0 or (depth == 0 and c == "}" and k != len(masked_item) - 1):
                return False
    if depth != 0:
        return False
    word = r"(?<![A-Za-z0-9_])" + re.escape(m.group(1)) + r"(?![A-Za-z0-9_])"
    return not re.search(word, masked_before)


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
    # The base element sequence must be a SUBSEQUENCE of the new one (keys:
    # template name for an invocation, exact text for anything else), and
    # every element not matched must be an insertable one. Matched greedily:
    # consuming an equal element is never worse than inserting it, so this
    # finds a match whenever one exists, independent of any line diff.
    i = 0
    for key, masked_item in ea:
        if i < len(eb) and key == eb[i][0]:
            i += 1
            continue
        if key[0] == "inv":
            continue
        if not _is_safe_inserted_fn(masked_item, masked_before):
            return False
    if i != len(eb):
        return False
    for name in tb:
        if sum(1 for k, _ in ea if k[:2] == ("inv", name)) < sum(
                1 for k, _ in eb if k[:2] == ("inv", name)):
            return False
    return True
