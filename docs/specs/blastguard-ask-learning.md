# blastguard Ask learning — spec (user ruling 2026-11-28)

User request (verbatim): 「これをもとにして、AskではなくAllow/Denyに判定していくこと、また継続的にこれをもとにして判定していくこと、その仕組みそのものをいれてほしい」

Rulings (binding, AskUserQuestion 2026-11-28):
- Learning unit = **shape**: `rule_id` + statically resolved **target class** + the command's **verbs**.
- Rulings of 2026-11-28 (second round, binding):
  - **R1** a verb is not just the basename: `basename [subcommand..] [option..]`, space separated. The subcommand (first non-option operand; two levels for gh/docker/kubectl/npm/…, and for git groups such as `stash`/`remote`/`worktree`) is included for multi-command tools. EVERY option is included, normalised (short cluster split `-rf` → `-f -r`, long option without `=value`, sorted). `git push --force` / `git branch -D` / `git add -A` / `rm -rf` therefore never share a key with `git push` / `git branch -d` / `git status` / `rm`. Operands that are not a subcommand (paths, refs) and operands the evaluator could not resolve (`rm "$f"`) add nothing, so `rm a` and `rm b` share a shape. Option VALUES are not in the key.
  - **R2** an inline interpreter program (`-c`/`-e` body, literal heredoc / here-string on stdin of python/perl/ruby/node/bash/sh/zsh …) adds `#<fnv1a64 hex of the body>` to the verb; same body → same key, changed body → different key. A program the evaluator cannot pin (wrapper such as `env python3 …`, `eval`, piped from a command) adds the unresolved token `?`: such a shape can learn a Deny but never an Allow.
  - **R3** protected config is judged from the detector's verdict, not only the target class: when the verdict's reason names a protected gate/config path (`blastguard::rule_id::reason_is_protected_config`: every rule id containing `protected`, or the text "protected gate/config path"), the shape's target class is raised to `protected-config` (unless it is already `system`). That is the single source of truth; `blastguard shape` prints it as `protected_config`.
  - **R4** `lookup`: when a table holds both an allow and a deny entry for one shape, **deny wins**.
  - **R5** everything under `~/.blastguard/learned/**` is protected config (PROTECTED_GLOBS): writes, removals and Write/Edit tool calls are never Allow, and a learned Allow can never unlock it. The learner (overwatch) writes there as its own process, not through the hook.
  - **R6** a hook-denied tool_result is not an approval (see Learner). Learner state version 2: a state cache of another version, or another blastguard version, is discarded and the table rebuilt, so tables built with the old (basename-only) keys are replaced. Shape schema tag: every class label in the key carries the schema tag, now `s3-` (`rule|s3-home|rm`; `s2-` was the previous one), so a key of an earlier schema can never equal a current key, in `lookup`, `learn` or `from_key` — an old table is inert, not wrongly matching, even before the learner rewrites it.
- **Allow** when the same shape was approved ≥3 times, by ≥2 distinct sessions, with 0 declines.
  **Deny** as soon as the shape has ≥1 decline.
  **System harm** and **protected config** (`~/.claude/settings*.json`, `~/.zshrc` / shell rc files, `.githooks/`, plugin cache bins/hooks) are **never auto-Allowed** (Deny learning still applies).
- Applied **immediately**: the learned table is regenerated deterministically from the records at each Stop (and on demand); blastguard reads it on its next decision.
- Every Allow promotion is also filed as a backlog item with a RED, to become a permanent rule fix (the table shrinks as fixes land).

Measured basis: ~/.blastguard/blocks/blocks.jsonl — 676 real interactive Asks (session ids are UUIDs; `probe`, `verify-*`, `v2`… are test/probe pseudo sessions and must be excluded), 610 approved+ran, 40 approved but the command errored (still an approval), 2 declined, 0 system harm. Replayed on 0.2.974: 588 still Ask.

- Rulings of 2026-11-28 (third round, schema `s3-`; verifier round 2 reach holes, tests `crates/blastguard/tests/verifier2_ask_learning_20261128.rs`):
  1. **Script file programs (R2 for files)**: `python3 /p/a.py`, `bash x.sh`, `node f.js` … add `#<fnv1a64 hex>` of the file's CURRENT content to the verb. **A file script is keyed by its content hash**: same content → same key, edited content → new key. Unreadable, missing, too large (> 1 MiB), relative-to-unknown-cwd, `.`/`..` spelled or unresolved paths add `?` (never Allow-learnable). The same applies to `interp - < file`.
  2. **Written then run**: a script written by a literal heredoc (`cat > f <<'EOF' … EOF`, no `cat` operands, `>` / `>|`) earlier on the same command line and then run has the HEREDOC BODY hashed into the run verb (rule 1's file is not read, it may be stale). A path written by anything else earlier on the line (append, `tee`, `cp`, …), or after an undetermined write, adds `?`.
  3. **Wrappers keep the wrapped verb**: `env`, `sudo`, `command`, `timeout`, `nice`, `nohup`, `xargs`, … keep the wrapped multi-command tool and its subcommand (`env git branch -D main` → `env git branch -D`), besides all options of the line.
  4. **Unpinned operands add `?`**: `xargs` (its arguments and flags come from stdin) and any command with an operand that is a `$(..)` / backquote substitution (a plain `$var` operand is unchanged: the pinned `rm "$f"` shape). Such a shape can learn a Deny, never an Allow.
  5. **Git writes take the class of the repository they act on**, not `none`: the effective cwd (after a literal `cd`, or `-C`); for `git worktree remove <path>` the path it deletes. A linked worktree / `~/.condukt/worktrees` / `.harness-worktrees` → `worktree`; a scratchpad → `scratchpad`; the MAIN working tree (`.git` is a directory, git-dir == common-dir) → new class **`main-tree`** (`s3-main-tree`), which, like `system` / `protected-config`, is never Allow-learnable (CLAUDE.md §8: main only receives merges). Not shown to be either (no `.git`, unreadable, `GIT_DIR`, `--git-dir`, non-literal cwd or subcommand, `clone` / `init`) → `undetermined`. Read-only subcommands (`status log diff show rev-parse …`, `branch` / `tag` / `remote` listings, `stash|worktree list`, `config --get`) keep `none`.
  6. The shape schema tag is `s3-`: every table learned with `s2-` keys is inert.

## Components

### 1. harness-core `asklearn` (pure, shared)
- `TargetClass`: `Worktree` (inside a git linked worktree or the session worktree / `~/.condukt/worktrees/...`), `Scratchpad` (under `/private/tmp/claude-*/…/scratchpad` or `$TMPDIR` session scratch), `ProtectedConfig`, `System` (SYSTEM_PREFIXES), `MainTree` (main working tree of a git repo; never Allow), `Home` (other paths under $HOME), `Undetermined` (target not statically resolvable), `None` (command has no write target).
- `Shape { rule_id: String, target_class: TargetClass, verbs: Vec<String> }` with canonical key string `"<rule_id>|<class>|<verb,verb,...>"` (verbs = sorted, deduped verb tokens (see R1/R2 below; formerly bare basenames) of every simple command; for non-Bash tools the verb is the tool name).
- Outcome record: `{ shape, session_id, outcome: Approved|Declined, ts, sample_command }`.
- `learn(records) -> LearnedTable` — deterministic (sorted keys; same input → byte-identical JSON):
  - declines ≥1 → `deny`
  - approvals ≥3 && distinct sessions ≥2 && declines == 0 && class ∉ {System, ProtectedConfig} && not system_harm → `allow`
  - otherwise no entry.
- `LearnedTable` JSON: `{"version":1,"entries":[{"shape":"…","decision":"allow"|"deny","approvals":N,"declines":M,"sessions":K,"samples":["cmd…" (≤3, truncated 300)]}]}` (no timestamps inside the table → determinism).
- `lookup(&table, &shape) -> Option<LearnedDecision>`.
- Reading the table returns a three-valued result: `Absent` (no file), `Undetermined` (unreadable / unparsable / wrong version), `Known(table)`.

### 2. blastguard (verdict side; read-only use of the table)
- When the final decision is **Ask** (main/interactive) — or a subagent Ask hardened to Deny — compute the shape and look it up:
  - `allow` entry → emit Allow; reason names the shape and counts (`learned: approved N times across K sessions, 0 declines — shape <key>`).
  - `deny` entry → emit Deny with the same kind of reason.
  - No entry, table Absent → unchanged Ask.
  - Table Undetermined → unchanged Ask (never Allow); reason gains a note that the learned table could not be read.
- Hard guards independent of the table: never turn a **Deny** into anything else; never Allow when `system_harm` is set or the target class is System/ProtectedConfig.
- Learned Allow applies to subagent mode as well (an Ask that would be hardened to Deny becomes Allow); learned Deny applies everywhere.
- Every decision record in blocks.jsonl gains `shape` (key string). Learned decisions are logged with `decision_kind:"learned"` and `original_decision:"ask"`.
- Table path: `~/.blastguard/learned/table.json`; override `BLASTGUARD_LEARNED_TABLE` (tests).
- New subcommand `blastguard shape`: reads a PreToolUse hook JSON on stdin, prints `{"decision":"ask|allow|deny","rule_id":…,"shape":"…","system_harm":bool}` without consulting the learned table (used by the learner to recompute shapes of historical Asks with the current analyser).

### 3. Learner (non-verdict; lives in overwatch per the 2026-11-17 scope ruling)
- `overwatch ask-learn [--blocks PATH] [--projects DIR] [--table PATH] [--blastguard BIN]`:
  - Reads interactive Ask records with UUID session ids.
  - Outcome by joining the session transcript `~/.claude/projects/*/<sid>.jsonl`: the tool_use with equal tool_name + tool_input; its tool_result containing "The user doesn't want to proceed with this tool use" → Declined; a tool_result that is a **hook denial** (text containing `hook error` / `hook blocking error` / `PreToolUse:`, or an `is_error` result carrying a `blastguard:` deny reason) → **excluded** (neither approval nor decline; user ruling 2026-11-28 R6: the call never ran and the user saw no prompt, so it is not an approval, CLAUDE.md §3); any other tool_result → Approved; no match / no result → excluded (not counted).
  - Shape: recomputed by `blastguard shape` on the stored tool_input+cwd; if the current analyser no longer Asks (allow/deny) the record is skipped.
  - Writes the table atomically (tmp + rename); unchanged content → no rewrite.
  - Incremental: a cursor (byte offset + outcome cache) so a Stop run stays fast; full rebuild with `--full`.
  - Prints a summary (`allow: N, deny: M, new allow promotions: …`).
  - Failure (log unreadable, blastguard missing, …): keep the previous table, exit non-zero, write the error to `~/.blastguard/learned/status.json`; the Stop hook prints it as a visible systemMessage (never silent).
- Stop hook in overwatch runs `ask-learn` (incremental) with a time budget.
- Promotions: each new Allow entry appended to `~/.blastguard/learned/promotions.jsonl` (`filed:false`).
- `overwatch ask-learn file-reds --worktree <linked worktree>`: for each unfiled promotion, writes `crates/blastguard/tests/learned_shape_<8hex>.rs` with an `#[ignore = "RED(learned …)"]` test asserting the sample commands are Allow with `BLASTGUARD_LEARNED_TABLE` pointing at an empty table, then `backlog add --repro-test "cargo test -p blastguard --test learned_shape_<8hex> -- --ignored"` and marks `filed:true`. Refuses on the main working tree.
