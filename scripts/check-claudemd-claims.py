#!/usr/bin/env python3
"""Verify that `path:line` claims in CLAUDE.md still describe reality.

Why this gate is SEPARATE from check-doc-claims.py
---------------------------------------------------
CLAUDE.md is an instruction/config file — the repo's operating norms — not a
documentation page. The generic doc-citation gate (`check-doc-claims.py`) used
to fold CLAUDE.md into its `docs/**/*.md` scope, which blurred two different
concerns under one gate: "does prose in docs/ still describe the code" and
"does the norm file the AGENT ITSELF is instructed to follow still describe the
code". This is the dedicated gate for the second concern: CLAUDE.md's policy
integrity. `check-doc-claims.py` no longer scans CLAUDE.md at all — see its
module docstring.

This gate does not duplicate the claim-extraction/verification engine. It
loads `check-doc-claims.py` (a sibling stdlib script; its filename has a
hyphen, so it is loaded via `importlib.util.spec_from_file_location` rather
than a normal `import`) and calls the SAME `scan()` / `extract_claims()` /
`check_claim()` functions, restricted to the single file `CLAUDE.md`. The
claim syntax, the finding kinds, the `doc-claim-exempt` marker, and the
whitespace/case/delimiter rules for quote matching are all identical to
check-doc-claims.py — see that module's docstring for the full contract.

Scope
-----
Exactly `CLAUDE.md` at the repository root. Nothing else.

A repository with NO CLAUDE.md is not a clean scope: an instruction file that
is supposed to exist but does not is a failure to observe the norm surface,
not "nothing to check". So a missing CLAUDE.md is Undetermined (exit 2), not
exit 0 — the same discipline check-doc-claims.py applies to an empty doc set.

Fail-closed contract — NO bypass flag
--------------------------------------
  exit 0  every claim in CLAUDE.md checks out
  exit 1  at least one unexempted finding                    -> block
  exit 2  the verdict could not be determined (missing file,
          unreadable file, cited file unreadable, etc.)      -> block

There is deliberately NO `--allow` / `--skip` / `--no-verify`-style flag on
this gate. A gate scoped correctly (one file, one purpose) should never need
an escape hatch of its own — the only sanctioned way to exempt a specific
claim is the same per-line marker check-doc-claims.py already supports:

    <!-- doc-claim-exempt: <reason> -->

immediately before the claim's line, with a mandatory, non-blank reason.
Exempted findings stay visible in `--json` output, exactly as in
check-doc-claims.py.
"""

from __future__ import annotations

import argparse
import importlib.util
import json
import os
import sys

_HERE = os.path.dirname(os.path.abspath(__file__))
_DOC_CLAIMS_PATH = os.path.join(_HERE, "check-doc-claims.py")


def _load_doc_claims_engine():
    """Load check-doc-claims.py by file path and return the loaded module.

    Its filename has a hyphen, so it cannot be reached with a normal `import
    check_doc_claims`; importlib.util.spec_from_file_location is the standard
    stdlib way to load a sibling script as a module without renaming it or
    duplicating its logic.
    """
    spec = importlib.util.spec_from_file_location(
        "_check_doc_claims_engine", _DOC_CLAIMS_PATH
    )
    if spec is None or spec.loader is None:
        raise ImportError(f"could not load spec for {_DOC_CLAIMS_PATH}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--repo", default=None)
    ap.add_argument("--json", action="store_true")
    args = ap.parse_args(argv)

    repo = args.repo or os.path.dirname(_HERE)

    try:
        engine = _load_doc_claims_engine()
    except (ImportError, OSError, SyntaxError) as exc:
        print(
            f"check-claudemd-claims: undetermined — could not load the shared "
            f"claim-verification engine from {_DOC_CLAIMS_PATH}: {exc}",
            file=sys.stderr,
        )
        if args.json:
            print(json.dumps({"verdict": "undetermined", "findings": []}))
        return 2

    try:
        # explicit=["CLAUDE.md"] restricts doc_set() to exactly that one file,
        # and — the property this gate depends on — doc_set() already raises
        # Undetermined if an explicitly named document does not exist. A
        # missing CLAUDE.md therefore resolves to exit 2 for free, via the
        # same code path check-doc-claims.py uses for a typo'd --doc.
        findings = engine.scan(repo, ["CLAUDE.md"])
    except engine.Undetermined as exc:
        print(f"check-claudemd-claims: undetermined — {exc}", file=sys.stderr)
        if args.json:
            print(json.dumps({"verdict": "undetermined", "findings": []}))
        return 2

    blocking = [f for f in findings if not f["exempt"]]
    verdict = "mismatched" if blocking else "clean"

    if args.json:
        print(json.dumps({"verdict": verdict, "findings": findings}))
    else:
        for f in findings:
            mark = "exempt" if f["exempt"] else "BLOCK"
            print(
                "[{}] {}:{} -> {}:{} {} ({})".format(
                    mark,
                    f["doc"],
                    f["doc_line"],
                    f["path"],
                    f["cited_line"],
                    f["kind"],
                    f["detail"],
                )
            )
        if blocking:
            print(
                "\ncheck-claudemd-claims: {} claim(s) in CLAUDE.md no longer "
                "match the code.\n"
                "Fix CLAUDE.md (or the code) IN THIS COMMIT. If a claim is\n"
                "deliberately historical, say so on the line before it:\n"
                "  <!-- doc-claim-exempt: <reason> -->\n"
                "There is no bypass flag for this gate.".format(len(blocking)),
                file=sys.stderr,
            )
        else:
            print("check-claudemd-claims: all CLAUDE.md claims match the tree.")

    return 1 if blocking else 0


if __name__ == "__main__":
    sys.exit(main())
