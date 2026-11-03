# refactor

A skill-only Claude Code plugin (no Rust crate, no hooks) for paying down an oversized
source file: split it one module per commit as **pure moves**, and prove every step
behaviour-preserving with a stdlib-only proof tool. It is the procedure used to split
blastguard's `detect.rs` (17742 -> 2402 lines). It is not a gate: nothing blocks on it;
the `/refactor` skill runs the proofs and stops on anything but exit 0.

- Skill: `/refactor <file>` — `skills/refactor/SKILL.md`
- Tool: `skills/refactor/scripts/refactor_proof.py` (python3, stdlib only)

## Proof tool

Every subcommand exits **0 = proven equal**, **1 = differs** (a human-readable
difference is printed), **2 = undetermined** (git failure, bad arguments, empty input
after normalisation, unparsable test output). Undetermined is never 0.

### Normalisation N (Rust, per line, both sides)

Strip whitespace; drop blank lines and `//!` lines; drop `use ...;` (including a
multi-line use up to its `;`) and `mod name;`, each with an optional `pub` /
`pub(crate)` / `pub(super)` / `pub(self)` / `pub(in path)` prefix, together with an
attribute line directly above them (e.g. `#[cfg(test)]`); remove one leading visibility
qualifier from every remaining line. N is line-based, not a Rust parser: a `use`/`mod`
line inside a multi-line string literal is dropped too, which is why the test-set
comparison runs alongside.

### `multiset --repo R --base B --head H -- <pathspec>...`

The multiset of N-lines over all files matching the git pathspecs at rev B equals that
at rev H: nothing was added, dropped, edited or duplicated. Prints a summary line
(file and line counts) and, on exit 1, the lines only at base (`-`) and only at head
(`+`). No file matching at either rev is exit 2. Line-based, so a rustfmt re-wrap shows
as a difference.

### `ordered --repo R --commit C --from <path> --to <path>`

The N-lines removed from `<from>` by commit C, in order, equal the N-lines of `<to>`, a
file C creates; and `<from>` gained nothing but use/mod lines (a line replaced in place
by itself modulo N — a visibility change — is not a gain). Compared as token sequences
in which whitespace is insignificant, brackets/commas are separate tokens, and a comma
ending a line before a line that starts with a closing bracket is dropped, so rustfmt
re-wrapping compares equal. `mod NAME { BODY }` removed and `NAME.rs` (or
`NAME/mod.rs`) created with BODY is also equal. `<to>` absent at C, present before C,
`<from>` absent before C, or C without a parent is exit 2.

### `tests record --repo R --out DIR -- <cmd...>` / `tests compare --repo R --baseline DIR [--allow-removed FILE] -- <cmd...>`

Runs the libtest-format command in R, parses **stdout only** (stderr is kept apart so
it cannot splice into result lines) for `test NAME ... ok|FAILED|ignored`, and records
the sorted passed / failed names to `DIR/pass.txt` / `DIR/fail.txt`. Names are kept as
a multiset, since the same path can exist in several test binaries. `compare` exits 0
iff the failed names equal the baseline's and the passed names equal the baseline's
minus the names listed in `--allow-removed`; otherwise 1, listing missing and new names.
The command's exit code is not part of the verdict (a baseline with known failures
exits non-zero too). No `test result:` line, or a `test X ...` line ending in anything
but ok / FAILED / ignored, is exit 2.

Tests: `python3 -m unittest scripts/test_refactor_proof.py` (repo root).
