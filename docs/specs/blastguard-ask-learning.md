# blastguard Ask learning — spec (user ruling 2026-11-28)

User request (verbatim): 「これをもとにして、AskではなくAllow/Denyに判定していくこと、また継続的にこれをもとにして判定していくこと、その仕組みそのものをいれてほしい」

Rulings (binding, AskUserQuestion 2026-11-28):
- Learning unit = **shape**: `rule_id` + statically resolved **target class** + the command's **verbs**.
- **Allow** when the same shape was approved ≥3 times, by ≥2 distinct sessions, with 0 declines.
  **Deny** as soon as the shape has ≥1 decline.
  **System harm** and **protected config** (`~/.claude/settings*.json`, `~/.zshrc` / shell rc files, `.githooks/`, plugin cache bins/hooks) are **never auto-Allowed** (Deny learning still applies).
- Applied **immediately**: the learned table is regenerated deterministically from the records at each Stop (and on demand); blastguard reads it on its next decision.
- Every Allow promotion is also filed as a backlog item with a RED, to become a permanent rule fix (the table shrinks as fixes land).

Measured basis: ~/.blastguard/blocks/blocks.jsonl — 676 real interactive Asks (session ids are UUIDs; `probe`, `verify-*`, `v2`… are test/probe pseudo sessions and must be excluded), 610 approved+ran, 40 approved but the command errored (still an approval), 2 declined, 0 system harm. Replayed on 0.2.974: 588 still Ask.

## Components

### 1. harness-core `asklearn` (pure, shared)
- `TargetClass`: `Worktree` (inside a git linked worktree or the session worktree / `~/.condukt/worktrees/...`), `Scratchpad` (under `/private/tmp/claude-*/…/scratchpad` or `$TMPDIR` session scratch), `ProtectedConfig`, `System` (SYSTEM_PREFIXES), `Home` (other paths under $HOME), `Undetermined` (target not statically resolvable), `None` (command has no write target).
- `Shape { rule_id: String, target_class: TargetClass, verbs: Vec<String> }` with canonical key string `"<rule_id>|<class>|<verb,verb,...>"` (verbs = sorted, deduped basenames of command words of every simple command; for non-Bash tools the verb is the tool name).
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
  - Outcome by joining the session transcript `~/.claude/projects/*/<sid>.jsonl`: the tool_use with equal tool_name + tool_input; its tool_result containing "The user doesn't want to proceed with this tool use" → Declined; any other tool_result → Approved; no match / no result → excluded (not counted).
  - Shape: recomputed by `blastguard shape` on the stored tool_input+cwd; if the current analyser no longer Asks (allow/deny) the record is skipped.
  - Writes the table atomically (tmp + rename); unchanged content → no rewrite.
  - Incremental: a cursor (byte offset + outcome cache) so a Stop run stays fast; full rebuild with `--full`.
  - Prints a summary (`allow: N, deny: M, new allow promotions: …`).
  - Failure (log unreadable, blastguard missing, …): keep the previous table, exit non-zero, write the error to `~/.blastguard/learned/status.json`; the Stop hook prints it as a visible systemMessage (never silent).
- Stop hook in overwatch runs `ask-learn` (incremental) with a time budget.
- Promotions: each new Allow entry appended to `~/.blastguard/learned/promotions.jsonl` (`filed:false`).
- `overwatch ask-learn file-reds --worktree <linked worktree>`: for each unfiled promotion, writes `crates/blastguard/tests/learned_shape_<8hex>.rs` with an `#[ignore = "RED(learned …)"]` test asserting the sample commands are Allow with `BLASTGUARD_LEARNED_TABLE` pointing at an empty table, then `backlog add --repro-test "cargo test -p blastguard --test learned_shape_<8hex> -- --ignored"` and marks `filed:true`. Refuses on the main working tree.
