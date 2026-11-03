# fde

Forward Deployed Engineer harness for Claude Code. For a foreign target
repository it infers a spec, has a human ratify it, denies edits to the target
until then, and records a per-requirement RED / GREEN evidence ledger outside
the target. A deterministic Rust binary plus the `/fde [target-dir]` skill.

**Status: rolled out and enabled** (parked and unparked on 2026-11-03 by user
ruling; see the note in `scripts/parked-plugins.json`).

**Platform:** Unix-like systems, including WSL. Windows is out of scope (user
ruling 2026-11-03; a scope definition, not a limitation).

Commands: `fde resolve`, `begin`, `ratify`, `status`, `forget`, `gate` (PreToolUse hook),
`evidence red|green|pass|verify`, `report`. Exit 2 always means undetermined
(deny for `gate`) and is never a pass. The verdict is `fde evidence verify`
(0 complete / 1 incomplete / 2 undetermined), which counts PASS as complete.
`fde report` prints the listing in every determined case and exits 0 only when
the target is ratified and every requirement is verified RED then GREEN; it
exits 1 (reason on stderr) for an unratified target (not registered, not
ratified, spec changed since ratification) or any unresolved, unmeasured or
PASS-only requirement, and 2 when undetermined.

`fde ratify` records the target HEAD commit and whether the work tree was dirty
(it needs at least one commit; otherwise exit 2), and every evidence record
carries the HEAD and dirty flag observed just before its command ran. Only
forward history counts: recording needs the ratification HEAD to be an
ancestor of (or equal to) the current HEAD, and `verify` / `report` need
ratification HEAD <= every counted record's HEAD <= current HEAD; a reset,
branch switch, rewritten history or a failed comparison is exit 2. A dirty tree
is recorded and shown by `report`, but is not disqualifying by itself. A
ratification from an earlier fde (no HEAD) still satisfies `status`, but the
evidence commands and `report` exit 2 until it is re-ratified.

Every evidence record also carries the ratification HEAD in force when it was
recorded (`ratified_head`), and only records of the current spec fingerprint
**and** the current ratification HEAD count. Re-ratifying at a different HEAD
therefore starts a new evidence epoch: earlier records are kept on disk but
ignored (counted on `report`'s `ignored:` line), and the requirements read as
unmeasured (exit 1) until RED / GREEN / PASS are recorded again. This is how a
human recovers after a reset off the ratified line; note that re-ratifying at
a forward commit also discards the earlier evidence from the answer.
Re-ratifying at the same HEAD keeps the records counting. A record from an
earlier fde that matches the fingerprint but has no `ratified_head` is not
placed in any epoch: recording, `verify` and `report` exit 2 while it exists.

Before ratification the gate denies Bash run in, or naming, the target. A
command is split into words by a small conservative lexer (quotes, backslash
escapes, `;&|()<>` operators, redirect targets, `~` as `$HOME`, the value of
`--opt=value` / `NAME=value`); each word is resolved against the payload cwd
and canonicalized (symlinks followed), so `../proj/x`, `rm -rf proj` from the
parent or `link/x` through a symlink are caught. While a target is guarded, a
word the gate cannot resolve (`$`, backtick, unquoted glob or `{`, `~user`, an
unterminated quote) or cannot canonicalize denies the call as undetermined.
Not seen: aliases / functions / `PATH` resolution, `cd` inside the command,
paths computed at run time; here-doc bodies are lexed as words. There is
one exception: an exact single `fde status`, `fde report` or `fde begin`
invocation (no chaining, redirects, substitution, quoting, env prefixes or
wrappers) is allowed. `fde ratify` is human-only: after the human approves,
they run it themselves (e.g. `! fde ratify [TARGET]` in their Claude Code
prompt, or in their own terminal); the model must not run it.

`fde forget [TARGET]` is human-only too: it removes the target's whole state
directory (registration, ratification, evidence; a corrupt registration
included) and never touches the target (0 removed / 1 not registered / 2
undetermined). While any target is registered, the gate denies every Bash
command that invokes `fde forget` (detected textually and conservatively). A
registered target whose root directory was deleted is "gone": it no longer
blocks unrelated work, but its former path stays denied with a reason naming
`fde forget`, and `fde status <root>` exits 1 saying `gone`. A root that cannot
be stat'ed (other than not-found), or a corrupt registration, still denies
everything.

State lives in `$FDE_STATE_DIR` (default `~/.fde`), keyed by the resolved git
top-level of the target.

Details (Japanese): [README.ja.md](README.ja.md); workflow:
[skills/fde/SKILL.md](skills/fde/SKILL.md).
