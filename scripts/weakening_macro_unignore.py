"""Un-ignoring macro invocations for `scripts/check-test-weakening.py`.

Proven-safe shape (`invocation_unignore_is_safe`), used only to WIDEN the set
of changes the gate may pass; anything not proven returns False and the gate
stays Undetermined (exit 2) or reports its ordinary findings.

A "forwarder" is a top-level `macro_rules! NAME` with exactly ONE arm whose
matcher starts with `$(#[$M:meta])*` and whose body contains
`$(#[$M])*` exactly once, with `$M` occurring nowhere else (matcher or body).
Whatever attributes an invocation passes are then only re-emitted as outer
attributes in that one place; dropping an `ignore` attribute among them can
only make a generated test run, never remove or weaken it.

The change is proven safe only when ALL hold:

* both sides tokenise and balance; every `macro_rules!` is a top-level item;
* the text of every `macro_rules!` definition is byte-identical on both sides
  and in the same order (so every template, forwarder or not, is unchanged);
* at least one forwarder exists, and every `NAME!` occurrence of a forwarder
  is a top-level invocation preceded by an item boundary (`;`, `}` or the
  start of the file) -- none is hidden in another macro or a fn body;
* replacing each forwarder invocation by a placeholder leaves the two files
  byte-identical (so all other code, comments aside inside the replaced span,
  is unchanged, and the same number of forwarder invocations sit in the same
  places);
* per invocation (matched by position), after stripping the LEADING
  `#[ignore]` / `#[ignore = ...]` attributes from its arguments, the opening
  delimiter, the remaining arguments (macro name, test ident and every other
  argument byte-for-byte after trimming) and the text after the closing
  delimiter are equal, and the ignore attributes at head are a subsequence of
  those at base: ignores may only be dropped, never added or altered.

What this does NOT see: it says nothing about invocations of non-forwarder
macros (those must be byte-identical) and an `#[ignore]` that is not leading
(it is then part of the compared remainder, so any change to it is not
proven).
"""

from __future__ import annotations

import re

from weakening_macro import (
    _ARM_END_RE,
    _ARM_RE,
    _ARROW_RE,
    _ATTR_NAME_RE,
    _ATTR_OPEN_RE,
    _INVOKE_RE,
    _MACRO_DEF_RE,
    _MACRO_RULES_RE,
    _close,
    _top_level_items,
)

_META_MATCHER_RE = re.compile(
    r"\A\s*\$\s*\(\s*#\s*\[\s*\$\s*([A-Za-z_][A-Za-z0-9_]*)\s*:\s*meta\s*\]\s*\)\s*\*"
)


def _forwarder(masked_def: str) -> bool:
    """True for a single-arm macro forwarding `$(#[$M:meta])*` exactly once."""
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
    mm = _META_MATCHER_RE.match(matcher)
    if not mm:
        return False
    var = re.escape(mm.group(1))
    body = inner[arrow.end():body_close]
    if len(re.findall(r"\$\s*" + var + r"(?![A-Za-z0-9_])", matcher + body)) != 2:
        return False
    return len(re.findall(r"\$\s*\(\s*#\s*\[\s*\$\s*" + var + r"\s*\]\s*\)\s*\*", body)) == 1


def _split_invocation(raw: str, masked: str, s: int, e: int):
    """`(key, ignores)` of the invocation at `[s, e)`, or None when its leading
    attributes do not balance. `ignores` are the whitespace-free texts of the
    leading `ignore` attributes; `key` is everything else that must not change."""
    m = _INVOKE_RE.match(masked, s)
    close = _close(masked, m.end() - 1) if m else None
    if m is None or close is None:
        return None
    k = m.end()
    ignores = []
    while True:
        w = k
        while w < close and masked[w].isspace():
            w += 1
        a = _ATTR_OPEN_RE.match(masked, w)
        if not a or a.end() > close:
            break
        ac = _close(masked, a.end() - 1)
        if ac is None or ac > close:
            return None
        n = _ATTR_NAME_RE.match(masked, a.end())
        if not n or n.group(1) != "ignore":
            break
        after_name = masked[n.end():ac].strip()
        if after_name and not after_name.startswith("="):
            break
        ignores.append(re.sub(r"\s+", "", raw[a.end():ac]))
        k = ac + 1
    key = (m.group(1), raw[m.end() - 1], raw[k:close].strip(), raw[close:e].strip())
    return key, ignores


def _normalise(raw: str, masked: str):
    """`(normalised_text, invocations)` or None when unprovable. `invocations`
    is `[(key, ignores)]` in file order; `normalised_text` is `raw` with each
    forwarder invocation replaced by a placeholder; `defs` are in the tuple."""
    items = _top_level_items(masked)
    if items is None:
        return None
    defs = [it for it in items if it[0] == "def"]
    if len(_MACRO_RULES_RE.findall(masked)) != len(defs):
        return None
    forwarders = {name for _, name, s, e, _b in defs if _forwarder(masked[s:e])}
    if not forwarders or len({n for _, n, *_ in defs}) != len(defs):
        return None
    invs = [it for it in items if it[0] == "inv" and it[1] in forwarders]
    for name in forwarders:
        occurrences = len(re.findall(r"(?<![A-Za-z0-9_])" + re.escape(name) + r"\s*!", masked))
        own = [it for it in invs if it[1] == name]
        if occurrences != len(own) or not all(it[4] for it in own):
            return None
    out, found, pos = [], [], 0
    for _, _name, s, e, _b in invs:
        parts = _split_invocation(raw, masked, s, e)
        if parts is None:
            return None
        out.append(raw[pos:s])
        out.append("\0INV\0")
        found.append(parts)
        pos = e
    out.append(raw[pos:])
    return "".join(out), found, [raw[s:e] for _, _, s, e, _ in defs]


def _is_subsequence(small: list, big: list) -> bool:
    it = iter(big)
    return all(any(x == y for y in it) for x in small)


def invocation_unignore_is_safe(before: str, after: str, masked_before: str,
                                masked_after: str) -> bool:
    """True only for the proven-safe shape in the module docstring."""
    if len(masked_before) != len(before) or len(masked_after) != len(after):
        return False
    b = _normalise(before, masked_before)
    a = _normalise(after, masked_after)
    if b is None or a is None:
        return False
    (tb, ib, db), (ta, ia, da) = b, a
    if tb != ta or db != da or len(ib) != len(ia):
        return False
    return all(kb == ka and _is_subsequence(igh, igb)
               for (kb, igb), (ka, igh) in zip(ib, ia))


def template_or_unignore_edit_is_safe(before: str, after: str, masked_before: str,
                                      masked_after: str, test_attr_re) -> bool:
    """Either proven-safe macro shape: the invocation-count/additive shape of
    `weakening_macro.template_invocation_edit_is_safe`, or the leading-`#[ignore]`
    removal shape of `invocation_unignore_is_safe` (2026-11-29). Lets the gate
    wire both through one call site (check-test-weakening.py is at its line cap)."""
    from weakening_macro import template_invocation_edit_is_safe
    return (template_invocation_edit_is_safe(before, after, masked_before,
                                             masked_after, test_attr_re)
            or invocation_unignore_is_safe(before, after, masked_before, masked_after))
