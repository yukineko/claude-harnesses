---
name: refactor
description: Pay down an oversized source file by splitting it one module per commit as pure moves, each proven behaviour-preserving. Use when asked to "split this file", "this file is too big", "pay down" a file, "refactor <file>", or when the check-file-growth.py pre-commit gate blocks a commit because a file grew past its cap.
argument-hint: "<path of the file to split>"
allowed-tools: Bash, Read, Edit, Write, Grep, Glob, AskUserQuestion
---

# refactor — pay down a large file, one proven pure move at a time

The procedure used for the blastguard refactor (`detect.rs` 17742 -> 2402 lines,
one module per commit). The proof tool lives next to this file:

```
P=<this skill dir>/scripts/refactor_proof.py   # python3 stdlib only
```

Every subcommand exits **0 = proven equal, 1 = differs (diff printed), 2 = undetermined**.
Exit 2 is never a pass: stop, report the output verbatim, and ask.

## 0. Isolate and measure

- Work in a linked worktree, never the main working tree (CLAUDE.md §8).
- Record the starting commit: `START=$(git rev-parse HEAD)`.
- Measure: `wc -l <file>`; if `scripts/check-file-growth.baseline` exists, note the
  file's cap there (`LIMIT` is 2000 lines).

## 1. Remove redundancy first — as separate, normal commits

Dead code and duplicates go away BEFORE any split, each in its own commit. These are
behaviour changes: they need tests and the normal TDD flow (RED first). Never mix
them into a move commit — a move commit must contain nothing but the move.

## 2. Baseline the test suite

```
python3 $P tests record --repo <wt> --out <scratch>/baseline -- cargo test -p <crate> --no-fail-fast
```

Only stdout is parsed (stderr can splice into `test X ... ok` lines). An intentional
RED repro test carries `#[ignore = "backlog <id>: ..."]`, so it is neither pass nor fail.

## 3. Split ONE module per commit, pure moves only

A pure move is cut/paste plus: `use`/`mod` lines, and visibility widening
(`pub(super)` etc.). No renames, no reformatting beyond `cargo fmt`, no "while I'm here".
Run `cargo fmt` and `cargo clippy -p <crate> --all-targets` before each commit.
After each commit:

```
python3 $P ordered --repo <wt> --commit HEAD --from <old file> --to <new file>
python3 $P tests compare --repo <wt> --baseline <scratch>/baseline -- cargo test -p <crate> --no-fail-fast
```

Both must exit 0.
- **exit 1** = the commit is not a pure move (lines reordered, edited, or the old file
  gained code). Fix it, or split the non-move part into its own tested commit.
- **exit 2** = undetermined (git failure, `--to` not newly created, no `test result:`
  line, garbled test line). Stop and report; never treat it as a pass.
- A test deliberately deleted by the move is listed in a file passed as
  `--allow-removed FILE`; anything else missing or new is exit 1.

`ordered` tolerates rustfmt re-wrapping (line breaks, a trailing comma before the
closing bracket) and an inline `mod name { ... }` moved to `name.rs`. The `tdd`
gate recognises pure moves: moved lines are not new implementation.

## 4. Prove the whole range

```
python3 $P multiset --repo <wt> --base $START --head HEAD -- <crate src path>
```

must exit 0. `multiset` is line-based: a signature that rustfmt re-wrapped after a
visibility change, or an inline test module turned into a file, shows up as a
difference. Exit 1 is still not a pass — if every listed line is such a re-wrap that a
per-commit `ordered` proof already covers, report the multiset output verbatim with
that explanation and let the human decide.

## 5. Lower the cap

In the same branch, lower the file's entry in `scripts/check-file-growth.baseline`
to the new size, or remove the entry when the file is now <= 2000 lines.

## 6. Reuse assessment (report only)

List the extracted modules other crates could use, with measured line counts and the
command/rev used (model: `docs/blastguard-reuse-assessment.md`). Do NOT move code across
crates in the same run — that is a separate, behaviour-visible change.

## Report

`git log --oneline $START..HEAD`, `wc -l` before/after, every proof command with its
exit code, and the reuse list. Anything not run is reported as not run.
