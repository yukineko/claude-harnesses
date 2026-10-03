#!/usr/bin/env python3
"""UserPromptSubmit hook: clear this session's maintree deny ledger (e033c406).

guard-maintree-bash.py / guard-maintree-edit.py record every refusal in
`~/.claude/state/maintree-deny/<session_id>.jsonl` (scripts/deny_ledger.py), and
both the PreToolUse retry check and the Stop-time change check read it. A new
human instruction is new authority: what the human now asks for may be exactly
the thing refused earlier, so the ledger starts empty again for the next turn.
Ledgers of other sessions untouched for more than 7 days are pruned (housekeeping
only; a failure there is ignored).

This hook judges nothing and never blocks the prompt: it always exits 0. If the
session's ledger could NOT be cleared, it says so on stdout (which Claude Code
adds to the model's context) so the stale refusals that will keep firing are
explained rather than silent. A stale ledger only ever makes the gates refuse
more, never less.

    exit 0   always
"""

from __future__ import annotations

import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))


def main() -> int:
    try:
        import deny_ledger  # noqa: E402
    except Exception as e:  # noqa: BLE001
        sys.stdout.write(f"maintree deny ledger was NOT cleared: cannot load "
                         f"scripts/deny_ledger.py ({type(e).__name__}: {e}).\n")
        return 0
    try:
        payload = json.load(sys.stdin)
    except (json.JSONDecodeError, UnicodeDecodeError, ValueError) as e:
        sys.stdout.write(f"maintree deny ledger was NOT cleared: unreadable "
                         f"hook payload ({e}).\n")
        return 0
    if not isinstance(payload, dict):
        sys.stdout.write("maintree deny ledger was NOT cleared: hook payload is "
                         "not a JSON object.\n")
        return 0
    note = deny_ledger.clear(payload)
    if note:
        sys.stdout.write(f"maintree deny ledger was NOT cleared: {note}.\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())
