#!/usr/bin/env python3
"""Verify the GATE_CRATES crate set is consistent across its 11 hardcoded sources.

Two related-but-distinct concepts are hardcoded across these sources:
  - "GATE crates": fleet defense gates that require a canary rollout
    (scripts/rollout-plugins.sh's GATE_CRATES= line is the source of truth).
  - "audit targets": crates continuous-audit reviews by default. This is a
    strict SUPERSET of GATE crates — it may include audit-only crates (e.g.
    `backlog`) that get reviewed but do not gate/block anything and so are
    NOT GATE crates (no canary requirement, not in pre-push's GATE_PATTERN).

Sources and how each must relate to the canonical GATE_CRATES set:
  - scripts/rollout-plugins.sh    GATE_CRATES="..."     (space-separated, canonical)
  - .githooks/pre-push            GATE_PATTERN='...'    (regex alternation) — must
    equal canonical EXACTLY (pre-push's canary advisory is GATE-crates-only).
  - scripts/continuous-audit.sh   DEFAULT_TARGETS="..." (comma-separated) — must be
    a SUPERSET of canonical (the audit target set; may include non-GATE crates).
  - scripts/check-plugin-rollout.py  module-level GATE_CRATES = (...) tuple —
    must equal canonical EXACTLY. It drives both that script's disabled-GATE-crate
    failure and its "add --canary for GATE crates: ..." fix hint, so a stale copy
    both under-guards and tells the reader to run a plain rollout for a crate
    that rollout-plugins.sh hard-rejects without --canary. (The hint used to be a
    second literal in the same file; it is now generated from this constant, so
    only the constant can drift.)
  - scripts/check-fail-open-mutation.py  module-level GATE_CRATES = (...) tuple —
    must equal canonical EXACTLY. This is the adversarial fail-open mutation
    harness: it hardcodes the same 6-crate list only because it is a standalone
    Python script that cannot `pub use harness_core::fleet::GATE_CRATES`. A
    stale copy here would either mutation-test a crate that is no longer a GATE
    crate, or (worse) silently skip a real GATE crate's fail-open coverage.
  - crates/harness-core/src/fleet.rs  pub const GATE_CRATES: &[&str] — must equal
    canonical EXACTLY. This is now the SOLE Rust-side literal: it used to be two
    independently hand-copied literals (crates/condukt/src/adversarial.rs's
    `pub const GATE_CRATES: [&str; N]`, deciding which completions are
    "high-stakes" enough to force the adversarial refutation panel, and
    crates/tdd/src/config.rs's `pub const GATE_CRATES: &[&str]`, deciding where
    `strict_separation` (RED/GREEN author diversity) defaults on). Both Rust
    copies had, at different points, silently lost `overwatch`, exempting the
    Continuous-Audit crate from the gates that loop depends on — the same
    failure mode this cross-source script exists to catch. `condukt` and `tdd`
    now each `pub use harness_core::fleet::GATE_CRATES;` instead of redefining
    the literal, so the Rust *compiler* (not this script) is what keeps those
    two crates' copies identical to this one; this script only needs to track
    this one Rust source against the non-Rust sources below.
  - crates/overwatch/skills/continuous-audit/SKILL.md  "## 対象 crate (既定)" section
    (comma-separated list after "既定の target は") — must equal
    scripts/continuous-audit.sh's DEFAULT_TARGETS EXACTLY (the doc must describe
    what the script actually defaults to, whatever audit-only crates it has).
  - docs/OVERVIEW.md  the "GATE_CRATES（a / b / c）" prose parenthetical in the
    Continuous-Audit section — must equal canonical EXACTLY. This is the
    human-facing description readers see before ever opening
    rollout-plugins.sh; a stale copy tells them the wrong crates require a
    canary rollout.
  - scripts/check-fail-open.py  module-level GATE_CRATES = (...) tuple — must
    equal canonical EXACTLY. This is the fail-open swallow scanner's own
    merge-blocking scope list (which crates' `src/` a fail-open finding
    blocks on, see that script's docstring); a stale copy would silently drop
    a real GATE crate from fail-open enforcement, or scan a crate that is no
    longer a GATE crate. Found as an untracked 9th copy (docs/gate-taxonomy.md
    "重複+実測ゼロ件" section, backlog bb667ce1) and left out of an earlier
    consolidation pass because it was live-enforcing (not "zero-observed");
    tracking it here does not change enforcement, only drift detection.

  - scripts/check-raw-io-ratchet.py  module-level GATE_CRATES = (...) tuple —
    must equal canonical EXACTLY. This is the raw-stdlib-I/O ratchet's scan
    scope: `iter_target_files()` walks `crates/<c>/src/**/*.rs` for exactly
    these crates, so a stale copy shrinks the ratchet's universe and the "floor
    held" line it prints describes a smaller fleet than exists. Found missing
    `taintguard` (backlog fb6b1796), i.e. the newest GATE crate's src/ was never
    ratcheted at all while the gate reported green.
  - CLAUDE.md  the "GATE クレート（a/b/c）" prose parenthetical in the plugin
    version/rollout section — must equal canonical EXACTLY. Same reasoning as
    docs/OVERVIEW.md, one level more load-bearing: CLAUDE.md is the norm file
    every session reads first, so a stale list there is the copy most likely to
    be believed. It also drifted to 6 in the same incident.

See docs/fix-gate-crates-drift.md for the incident that motivated this checker.

The 2026-08-04 recurrence (backlog fb6b1796) was not a failure of the comparison
logic but of its ROSTER: three of the four places a reader/tool actually consults
(.githooks/pre-push, scripts/check-raw-io-ratchet.py, CLAUDE.md) said 6 while
canonical said 7, and only the first of those three was a registered source. The
other two were invisible here, so the checker printed a green "consistent across
9 sources" over live drift. Two lessons are baked in below: every NEW hardcoded
copy must be appended to SOURCES in the same commit that creates it, and this
script must be RUN by a hook — it spent this whole period wired to nothing but a
CI workflow file that does not exist in this repo, which is why the drift it was
built to catch survived in-tree for weeks. It is now a .githooks/pre-commit gate.

Exit 0 if all sources satisfy their required relation, 1 on any drift.
Run from the repo root:  python3 scripts/check-gate-crates-sync.py
"""
import os
import re
import sys

REPO = os.getcwd()


def canonical_crates(text):
    """Extract the GATE_CRATES="..." value (space-separated) from rollout-plugins.sh."""
    m = re.search(r'^GATE_CRATES="([^"]+)"', text, re.M)
    if not m:
        return None
    return set(m.group(1).split())


def continuous_audit_crates(text):
    """Extract the DEFAULT_TARGETS="..." value (comma-separated) from continuous-audit.sh."""
    m = re.search(r'^DEFAULT_TARGETS="([^"]+)"', text, re.M)
    if not m:
        return None
    return set(x for x in m.group(1).split(",") if x)


def pre_push_crates(text):
    """Extract crate names from the GATE_PATTERN='^crates/(a|b|c)/' regex."""
    m = re.search(r"^GATE_PATTERN='\^crates/\(([^)]+)\)/'", text, re.M)
    if not m:
        return None
    return set(m.group(1).split("|"))


# Comment syntaxes to blank out of a source file before locating the constant
# and scraping quoted crate names out of it. Scraping RAW text counts a
# commented-out entry (`// "overwatch",` / `# "overwatch",`), a prose TODO that
# merely names the crate, or — worse — a doc comment that RESTATES the whole
# constant, as if it were live code. A constant that actually lost a gate then
# still parses to the full canonical set and the checker prints a green "OK":
# a silent false negative in exactly the drift this checker exists to catch.
_RUST_COMMENT = r"/\*.*?\*/|//[^\n]*"
_PYTHON_COMMENT = r"#[^\n]*"

# String-literal syntaxes, alternated BEFORE the comment pattern so a comment
# marker inside a string is never mistaken for a comment. Language-specific on
# purpose: Python has single-quoted and triple-quoted strings, while Rust's `'`
# is a lifetime marker far more often than a char literal, so treating `'…'` as
# a string there would swallow real code.
_PYTHON_STRING = (
    r'"""(?:[^"\\]|\\.|"(?!""))*"""'
    r"|'''(?:[^'\\]|\\.|'(?!''))*'''"
    r'|"(?:\\.|[^"\\\n])*"'
    r"|'(?:\\.|[^'\\\n])*'"
)
_RUST_STRING = r'"(?:\\.|[^"\\])*"'


def _strip_comments(text, comment_pattern, string_pattern):
    """Replace comments in `text` with a space, leaving string literals intact.

    Alternating the string-literal pattern FIRST means a comment marker that
    appears inside a quoted string is matched as part of that string and left
    alone, so we never truncate a real entry.
    """
    pattern = re.compile(f"(?P<s>{string_pattern})|(?:{comment_pattern})", re.S)
    return pattern.sub(lambda m: m.group("s") if m.group("s") is not None else " ", text)


def _sole_match(pattern, text):
    """The single match of `pattern` in `text`, or None if there are zero or
    MORE THAN ONE.

    Ambiguity is deliberately fail-closed. Taking the FIRST match (what
    `re.search` does) silently picks a winner between two indistinguishable
    candidates. When the loser is the live definition and the winner is a stale
    copy in a docstring that still spells out the healthy set, the checker
    reports the copy's set and prints OK for a constant that has drifted. There
    is no way to tell code from prose with a regex, so the only safe answer is
    to refuse: the caller treats None as drift, which is a loud false positive
    at worst and never a silent pass.
    """
    matches = list(pattern.finditer(text))
    return matches[0] if len(matches) == 1 else None


_PYTHON_CONST_RE = re.compile(r"^GATE_CRATES\s*=\s*\(([^)]*)\)", re.M | re.S)
_RUST_CONST_RE = re.compile(
    r"pub const GATE_CRATES\s*:[^=]*=\s*&?\[(.*?)\]\s*;", re.S
)


def python_const_crates(text):
    """Extract crate names from a module-level `GATE_CRATES = (...)` tuple.

    check-plugin-rollout.py used to hardcode the GATE list a second time inside
    its `--canary for GATE crates: a/b/c)` drift-hint prose. That literal is
    gone: the file now holds ONE copy in a module-level constant and generates
    the hint text from it, so the hint cannot drift from the constant at all —
    only the constant can drift from canonical, which is what we parse here.

    `#` comments are stripped from the WHOLE FILE before the constant is
    located, not merely from the captured span. Stripping afterwards cannot help
    when the comment itself supplies the entire span.

    Returns None if the constant is missing, unrecognizable, or AMBIGUOUS (more
    than one candidate definition — see `_sole_match`). The caller treats None
    as drift: loud and fail-closed is the right failure mode, since an
    unparseable source is exactly the state where the check protects nothing.
    """
    stripped = _strip_comments(text, _PYTHON_COMMENT, _PYTHON_STRING)
    m = _sole_match(_PYTHON_CONST_RE, stripped)
    if not m:
        return None
    crates = set(re.findall(r'"([a-z0-9_-]+)"', m.group(1)))
    return crates or None


def rust_const_crates(text):
    """Extract crate names from a Rust `pub const GATE_CRATES ... = [ ... ];`.

    Handles both shapes the regex may see: the sized array
    (`pub const GATE_CRATES: [&str; 6] = [...]`) and the slice
    (`pub const GATE_CRATES: &[&str] = &[...]`, the shape
    crates/harness-core/src/fleet.rs actually uses today). The `&` before the
    bracket is optional and the array length in the type is ignored — the
    parsed membership is the thing under test, and a wrong length is a compile
    error anyway.

    Until this constant was consolidated into crates/harness-core/src/fleet.rs,
    it was two independently hand-copied Rust literals
    (crates/condukt/src/adversarial.rs and crates/tdd/src/config.rs). Both were
    invisible to this checker for a while and both silently drifted, missing
    `overwatch`: editing crates/overwatch/** triggered neither condukt's
    adversarial panel nor tdd's default strict_separation, so the crate
    implementing the Continuous-Audit loop was exempt from the very gates that
    loop depends on. `condukt` and `tdd` now `pub use` the single
    harness-core copy instead of redefining the literal, so this function only
    needs to track the one remaining Rust source
    (crates/harness-core/src/fleet.rs) — the Rust compiler keeps the two `pub
    use` re-exports identical to it by construction, which this script cannot
    silently fail to notice the way it could a second hand-copied literal.

    `//` and `/* … */` comments are stripped from the WHOLE FILE before the
    constant is located. Stripping only the captured span was not enough: this
    file carries a long "keep in sync with rollout-plugins.sh" doc comment
    directly above the constant, and one illustrative
    `/// pub const GATE_CRATES: [&str; 6] = ["…", "overwatch"];` line is matched
    INSTEAD of the constant, with its own body as the span — no comment marker
    inside it left to strip.

    Returns None if missing, unrecognizable, or ambiguous (see `_sole_match`).
    """
    stripped = _strip_comments(text, _RUST_COMMENT, _RUST_STRING)
    m = _sole_match(_RUST_CONST_RE, stripped)
    if not m:
        return None
    crates = set(re.findall(r'"([a-z0-9_-]+)"', m.group(1)))
    return crates or None


def claudemd_crates(text):
    """Extract crate names from CLAUDE.md's prose "GATE クレート（a/b/c）" list.

    Deliberately keyed on the Japanese phrase `GATE クレート` rather than on the
    identifier `GATE_CRATES` that overview_md_crates() looks for: CLAUDE.md
    spells the concept in prose and mentions the identifier nowhere, so reusing
    the OVERVIEW extractor here would return None (parsed as "drift") for a file
    that is perfectly in sync. Prose, like OVERVIEW's, so `_sole_match` is not
    applied — but a missing or empty match still returns None and the caller
    treats that as drift rather than as an absent file.
    """
    m = re.search(r"GATE\s*クレート\s*(?:（|\()([^）)]+)(?:）|\))", text)
    if not m:
        return None
    crates = set(x.strip() for x in m.group(1).split("/") if x.strip())
    return crates or None


def skill_md_crates(text):
    """Extract crate names from the "## 対象 crate (既定)" section's backtick CSV."""
    m = re.search(r"##\s*対象\s*crate\s*\(既定\)\s*\n+.*?`([a-z0-9_,-]+)`", text, re.S)
    if not m:
        return None
    return set(x for x in m.group(1).split(",") if x)


def overview_md_crates(text):
    """Extract crate names from docs/OVERVIEW.md's prose "GATE_CRATES（a / b / c）" list.

    This is the human-facing description of the canonical set inside the
    Continuous-Audit section. It is prose, not code, so `_sole_match` is not
    applied here (there is no comment/string ambiguity to guard against for a
    single fenced-off parenthetical) — but a missing or empty match still
    returns None, which the caller treats as drift rather than as an
    unrelated/absent file.
    """
    m = re.search(r"GATE_CRATES(?:（|\()([^）)]+)(?:）|\))", text)
    if not m:
        return None
    crates = set(x.strip() for x in m.group(1).split("/") if x.strip())
    return crates or None


# mode:
#   "canonical" — this source defines the canonical GATE_CRATES set.
#   "exact"     — must equal canonical exactly (no extra, no missing).
#   "superset"  — must be a superset of canonical (may include audit-only
#                 crates that are reviewed but are not GATE crates).
#   "mirror:<path>" — must equal the named source's parsed set exactly
#                 (a doc describing what that source actually contains).
SOURCES = [
    ("scripts/rollout-plugins.sh", canonical_crates, "canonical"),
    (".githooks/pre-push", pre_push_crates, "exact"),
    ("scripts/continuous-audit.sh", continuous_audit_crates, "superset"),
    ("scripts/check-plugin-rollout.py", python_const_crates, "exact"),
    ("scripts/check-fail-open-mutation.py", python_const_crates, "exact"),
    ("crates/harness-core/src/fleet.rs", rust_const_crates, "exact"),
    (
        "crates/overwatch/skills/continuous-audit/SKILL.md",
        skill_md_crates,
        "mirror:scripts/continuous-audit.sh",
    ),
    ("docs/OVERVIEW.md", overview_md_crates, "exact"),
    ("scripts/check-fail-open.py", python_const_crates, "exact"),
    ("scripts/check-raw-io-ratchet.py", python_const_crates, "exact"),
    ("CLAUDE.md", claudemd_crates, "exact"),
]


def check(repo=REPO, sources=SOURCES):
    """Return (ok, canonical_set, [(path, mode, extracted_set_or_None), ...]) for the given repo."""
    parsed = []
    by_path = {}
    canonical = None
    for rel_path, extractor, mode in sources:
        path = os.path.join(repo, rel_path)
        if not os.path.isfile(path):
            parsed.append((rel_path, mode, None))
            by_path[rel_path] = None
            continue
        with open(path, encoding="utf-8") as f:
            text = f.read()
        crates = extractor(text)
        parsed.append((rel_path, mode, crates))
        by_path[rel_path] = crates
        if mode == "canonical":
            canonical = crates

    if canonical is None:
        return False, None, parsed

    def satisfies(crates, mode):
        if crates is None:
            return False
        if mode in ("canonical", "exact"):
            return crates == canonical
        if mode == "superset":
            return canonical <= crates
        if mode.startswith("mirror:"):
            target = by_path.get(mode.split(":", 1)[1])
            return target is not None and crates == target
        raise ValueError(f"unknown mode: {mode}")

    ok = all(satisfies(crates, mode) for _, mode, crates in parsed)
    return ok, canonical, parsed


def _mismatch_detail(crates, mode, canonical, by_path):
    if crates is None:
        return "could not parse a crate set"
    if mode in ("canonical", "exact"):
        missing = canonical - crates
        extra = crates - canonical
    elif mode == "superset":
        missing = canonical - crates
        extra = set()
    elif mode.startswith("mirror:"):
        target_path = mode.split(":", 1)[1]
        target = by_path.get(target_path)
        if target is None:
            # `or set()` here used to turn an unparseable TARGET into an empty
            # set, so every crate this file legitimately mirrors was reported as
            # `unexpected [...]` — blaming the mirror for the target's breakage
            # and sending the reader to the wrong file to fix it.
            return f"cannot compare: {target_path} did not parse"
        missing = target - crates
        extra = crates - target
    else:
        return "unknown mode"
    detail = []
    if missing:
        detail.append(f"missing {sorted(missing)}")
    if extra:
        detail.append(f"unexpected {sorted(extra)}")
    return "; ".join(detail) if detail else "ok"


def main():
    ok, canonical, parsed = check(repo=os.getcwd())
    if canonical is None:
        print("check-gate-crates-sync: could not parse canonical GATE_CRATES from "
              "scripts/rollout-plugins.sh", file=sys.stderr)
        return 1

    if ok:
        audit_targets = next(
            (crates for path, mode, crates in parsed if mode == "superset"), canonical
        )
        print(f"OK: GATE_CRATES consistent across {len(parsed)} sources: "
              f"{','.join(sorted(canonical))} (audit targets: {','.join(sorted(audit_targets))})")
        return 0

    by_path = {rel_path: crates for rel_path, _mode, crates in parsed}
    print("FAIL: GATE_CRATES definition drift detected", file=sys.stderr)
    print(f"  canonical (scripts/rollout-plugins.sh): {sorted(canonical)}", file=sys.stderr)
    for rel_path, mode, crates in parsed:
        detail = _mismatch_detail(crates, mode, canonical, by_path)
        if detail != "ok":
            shown = sorted(crates) if crates is not None else None
            print(f"  {rel_path} [{mode}]: {shown} ({detail})", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
