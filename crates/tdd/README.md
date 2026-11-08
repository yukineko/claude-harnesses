# tdd

> **Test-first gate for Claude Code**, written in Rust.

Agents skip tests, and "test-first" is usually just a claim. `tdd` makes both
**enforced** and **verifiable**:

| Surface | When | Guarantees |
|---|---|---|
| **`tdd gate`** (Stop hook) | every Stop | implementation code can't land **without a test** — a blocked stop feeds the reason back so the agent writes one |
| **`tdd red` / `green` / `verify`** | driven by `/tdd` | **test-first**: `red` requires the tests to *fail* first and records a proof; `green` requires a prior `red` then a *pass*; `verify` confirms RED→GREEN happened |

It is the *test-first* sibling of the harness's other gates:

| Gate | When | Asks |
|---|---|---|
| `precommit-audit` | pre-commit | does the diff obey policy? (static) |
| **`tdd`** | **on Stop / in /tdd** | **was a test written, first?** |
| `donegate` | on Stop | does it actually build & pass? (runs it) |
| `specguard` | on demand | did the impl drift from spec? (LLM) |

No API key. `tdd` is a deterministic Rust binary that reads `git` and spawns the
test command; the LLM labor (writing the test, implementing) runs in your Claude
Code subscription.

## How the Stop gate works

On each stop `tdd gate`:

1. reads the hook JSON from stdin (`session_id`, `cwd`, `transcript_path`) and
   judges **only the hook `cwd`'s checkout** (user ruling 2026-11-08: a gate must
   not look at, verify or test any other worktree; nothing judges them in its place).
   A file the session wrote into another worktree is neither scanned nor credited
   here. A test this session wrote (per its transcript) and committed since the
   session started in this checkout is credited to its implementation change (the
   credit is printed on stderr); a test in another worktree is never credited. An
   undetermined attribution (no / unreadable transcript) or an undetermined
   session start credits nothing. A missing-test block names the checkout path it
   judged;
2. loads `./tdd.toml` (or `~/.tdd/config.toml`, or language-aware defaults);
3. asks `git` what changed and which lines were **added**, then keeps only the
   files **this session wrote**: a file counts as this session's when its
   transcript (or a subagent's sidechain transcript) records an
   `Edit`/`Write`/`MultiEdit`/`NotebookEdit` of it, or a `Bash` command naming
   its path outside a read-only statement — merely reading a file
   (`git diff`, `grep`, `cat`, naming it in a `tdd skip --reason`) does not make
   it this session's (`harness_core::attribution::attribute_own_writes`, shared
   with reviewgate). Other uncommitted files — e.g. a concurrent session's — are
   excluded and named on stderr and in the block reason as "not attributed to
   this session"
  .
   If the transcript is missing or unreadable, attribution is undetermined and
   the **whole checkout** is scanned, with the reason saying authorship was not
   verified
  .
   Known gap: a file written by a command that never names it (`cargo fmt`, a
   shell glob, a script) is excluded too;
4. counts *added implementation lines* (impl-glob files, minus test files and
   inline test markers) and looks for **test evidence** (an added `#[test]` /
   `def test_` / `func Test…` / `it(...)`, or a changed file under `tests/`).
   **Pure moves are recognised automatically**: an added impl line is not
   counted when it is part of a *run* — at least 3 consecutive added lines of
   one file (one diff hunk, or a whole new untracked file) that equal, in the
   same order, consecutive lines *removed from one impl file* (one hunk) in
   the same change set (working tree + index + this session's commits). A
   multiset: each removed line cancels at most one added line, so a copy
   (nothing removed) still counts. Matching normalisation: in brace languages
   (`rs go c h cc cpp hpp java js jsx ts tsx mjs cjs cs swift kt kts scala php
   dart`) the line is trimmed and one leading `pub` / `pub(..)` is dropped; in
   every other file (Python, YAML, no extension, …) only trailing whitespace
   is dropped, because there a dedent changes control flow. Lines that are
   only a `use …;` statement (multi-line too) or a `mod name;` declaration
   are not impl lines; they and blank lines are skipped inside a run (neither
   counted toward it nor breaking it). Removals from docs or test files never
   cancel. The number of lines read as moved is printed to stderr on every
   run. **Known limits:** (a) a block of 3+ lines moved to another place in
   its own file (or to another file) reads as a move even where the new
   position changes behaviour (e.g. statements reordered as whole blocks);
   (b) matching is on text only, so 3+ consecutive lines of boilerplate
   deleted in one place can vouch for the same 3+ lines added elsewhere;
   (c) a run is 3 lines, so a moved 1–2 line function still counts as new;
   (d) a lone path-shaped statement right after a dangling added
   `use x::{` is swallowed as part of the `use`;
5. **impl added + no test** → `{"decision":"block","reason":…}` so Claude writes
   a test and continues; otherwise the stop proceeds.

A per-session attempt counter gives up after `max_attempts` so a stuck agent is
never trapped. Escape hatch, for genuine refactors that edit lines (a pure
move no longer needs it — see step 4) and docs: `tdd skip
--reason "<why>"` — reason required, scoped to the issuing session, consumed
once, and recorded in the gate log at both issue and consumption. (The old shared
`.tdd-skip` file in the project root was removed: a one-shot marker in a shared
tree is consumed by whichever session stops next — CLAUDE.md section 5.) Kill
switch: `TDD_DISABLE=1`, effective only in the environment Claude Code itself was
started with, since the hook inherits the app's environment.

## Test-first proof (`/tdd` skill)

```
/tdd <behaviour you want>
  Phase 1  design the API (stubs: todo!())
  Phase 2  write the tests
  Phase 3  tdd red   --task <id>   →  RED  (must fail)   → .tdd/<id>.red.json
  Phase 4  implement
  Phase 5  tdd green --task <id>   →  GREEN (must pass)  → .tdd/<id>.green.json
  Phase 6  tdd verify --task <id>  →  RED→GREEN verified
```

`tdd red` refuses to record a proof if the tests already pass (that isn't
test-first); `tdd green` refuses without a prior RED proof. The test command
comes from `--cmd`, else `tdd.toml`'s `test_cmd` (default `cargo test`).

`tdd oracle --task <id>` classifies the task's RED→GREEN transition from its
proofs and prints a JSON report (`{"transition":"fail_to_pass","valid_fp_oracle":true,…}`);
it exits 0 only for a valid Fail→Pass oracle. A valid oracle requires the RED
and GREEN proofs to have been recorded with the same test command (`cmd`): a
RED of one command followed by a GREEN of a different command (or a proof with
no recorded `cmd`) reports `transition:"command_mismatch"`,
`valid_fp_oracle:false` (exit 1). A missing/corrupt proof reports
`transition:"unknown"` (exit 1); a proof that exists but cannot be read reports
`transition:"undetermined"`, `valid_fp_oracle:false` with the read error under
`"undetermined"` (exit 2) — never as a missing or valid proof. It isn't part of
the `/tdd` skill's own phases — it's the machine oracle `condukt`'s Fail→Pass
gate (`condukt state check-oracle`) calls to verify a `fix`/`feature` task's
RED/GREEN proofs deterministically.

## Config & other subcommands

`tdd` reads `./tdd.toml` (trusted projects only — its `test_cmd` is executed
verbatim), else `~/.tdd/config.toml`, else language-aware defaults. Common keys:

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `true` | master switch for the gate |
| `mode` | `"strict"` | `"strict"`: report the finding on every stop. `"loose"` (for projects that will not or cannot do test-first): warn once per implementation file content hash — a stop whose flagged files are all at an already-warned hash goes through silently (gate log `already-warned`); editing a file warns again. An undetermined scan still blocks every time. Set it per repository in that repo's `tdd.toml` |
| `max_attempts` | `3` | give up (allow the stop) after N consecutive blocks |
| `reset_after_secs` | `600` | idle gap that resets the attempt counter |
| `min_added_impl_lines` | `1` | added impl lines before a test is required |
| `test_cmd` | `cargo test` | default command for `tdd red` / `tdd green` |
| `proof_dir` | `.tdd` | where RED/GREEN proofs are written |

`impl_globs` / `test_path_globs` / `test_markers` override the language-aware
file/marker defaults.

### Strict test/impl author separation (`strict_separation`)

`tdd red --author <id>` records who wrote the test into the RED proof; `tdd green
--author <id>` is compared against it. With strict separation on, the same identity
(case/whitespace-insensitive), or a missing `--author` on either side, makes `tdd
green` **refuse** (fail-closed), so one agent cannot write both a wrong
implementation and a test bent to match it. Effective value, highest first:
`--strict-separation` / `--no-strict-separation` on `tdd green` (mutually
exclusive), then `strict_separation` in the config, then **on by default inside a
gate crate** (`crates/<gate>/…`, per `harness_core::fleet::GATE_CRATES`), else off.

Other subcommands: `tdd skip --reason "<why>"` (one-shot, session-scoped; see
above), `tdd init` (starter `tdd.toml`), `tdd status` (show the resolved config +
what the gate would do), `tdd trust` (honor this project's `tdd.toml` `test_cmd`,
untrusted by default), `tdd oracle --task <id>` (classify RED→GREEN and print
a JSON verdict — see above), `tdd uninstall` (remove the Stop hook).

## Install

Via the plugin marketplace (wires the Stop hook through `hooks/hooks.json`, with
a 30s timeout, and ships the `/tdd` skill). For non-plugin use: `tdd install`
merges the Stop hook into `~/.claude/settings.json`; `tdd init` writes a starter
`tdd.toml`.
