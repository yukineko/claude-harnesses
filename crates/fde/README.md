# fde

Forward Deployed Engineer harness for Claude Code. For a foreign target
repository it infers a spec, has a human ratify it, denies edits to the target
until then, and records a per-requirement RED / GREEN evidence ledger outside
the target. A deterministic Rust binary plus the `/fde [target-dir]` skill.

**Status: parked** (declared in `scripts/parked-plugins.json`; not rolled out,
not enabled).

Commands: `fde resolve`, `begin`, `ratify`, `status`, `gate` (PreToolUse hook),
`evidence red|green|pass|verify`, `report`. Exit 2 always means undetermined
(deny for `gate`) and is never a pass. The verdict is `fde evidence verify`
(0 complete / 1 incomplete / 2 undetermined); `fde report` is a listing whose
exit code is not a verdict (it exits 0 for an unratified target too).

Before ratification the gate denies Bash run in, or naming, the target, with
one exception: an exact single `fde status`, `fde report` or `fde begin`
invocation (no chaining, redirects, substitution, quoting, env prefixes or
wrappers) is allowed. `fde ratify` is human-only: after the human approves,
they run it themselves (e.g. `! fde ratify [TARGET]` in their Claude Code
prompt, or in their own terminal); the model must not run it.

State lives in `$FDE_STATE_DIR` (default `~/.fde`), keyed by the resolved git
top-level of the target.

Details (Japanese): [README.ja.md](README.ja.md); workflow:
[skills/fde/SKILL.md](skills/fde/SKILL.md).
