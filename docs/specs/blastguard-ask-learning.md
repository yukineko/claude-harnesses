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
  - **R6** a hook-denied tool_result is not an approval (see Learner). Learner state version 2: a state cache of another version, or another blastguard version, is discarded and the table rebuilt, so tables built with the old (basename-only) keys are replaced. Shape schema tag: every class label in the key carries the schema tag, now `s4-` (`rule|s4-home|rm`; `s3-` was the previous one), so a key of an earlier schema can never equal a current key, in `lookup`, `learn` or `from_key` — an old table is inert, not wrongly matching, even before the learner rewrites it.
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
  5. **Git writes take the class of the repository they act on**, not `none`: the effective cwd (after a literal `cd`, or `-C`); for `git worktree remove <path>` the path it deletes. The cwd's own linked worktree (git's answer, not a path substring; another linked worktree is `undetermined`, round 10) → `worktree`; a scratchpad → `scratchpad`; the MAIN working tree (`.git` is a directory, git-dir == common-dir) → new class **`main-tree`** (`s3-main-tree`), which, like `system` / `protected-config`, is never Allow-learnable (CLAUDE.md §8: main only receives merges). Not shown to be either (no `.git`, unreadable, `GIT_DIR`, `--git-dir`, non-literal cwd or subcommand, `clone` / `init`) → `undetermined`. Read-only subcommands (`status log diff show rev-parse …`, `branch` / `tag` / `remote` listings, `stash|worktree list`, `config --get`) keep `none`.
  6. The shape schema tag is `s3-`: every table learned with `s2-` keys is inert.
- Rulings of 2026-11-28 (fourth round; verifier round 3 reach holes, tests `crates/blastguard/tests/verifier3_ask_learning_20261128.rs`). The `s3-` tag is kept: the RED tests pin the `s3-` class labels, and a table learned before this round is only wrong for the git / script shapes below (which now carry `?` or a different class), not mis-keyed.
  1. **Wrappers keep the repository class.** `env` / `command` / `timeout` / `nice` / `nohup` / `sudo` (and any wrapper in the verb's wrapper list) in front of `git`: the wrapped `git` is judged after the wrapper is stripped (first `git` operand; `env VAR=..` assignments count as the git environment). `env git add -A` in the main tree is `s3-main-tree`.
  2. **A git write whose repository cannot be named is never Allow-learnable.** `cd -`, `pushd`, a non-literal cwd, `-C $(..)`, `--namespace`, an unresolved `GIT_*` override, `clone` / `init`, an attached `-C<path>` (not git syntax) add `?` to the verb. So does a repository that was named but is not shown to be a main or linked working tree (nonexistent directory, `-C ../` landing outside any repo): `shape_of` adds `?` when `class_of_repo` is `undetermined`. `cd` is tracked: the evaluator now resolves a relative `cd` (including `..`) against the payload cwd (`bash_facts(command, cwd)`), and `cd x; git ..` (the `cd` may have failed) judges every candidate directory.
  3. **Environment overrides.** `GIT_DIR` / `--git-dir` naming a literal `<repo>/.git` classes `<repo>` as well as the cwd; `GIT_WORK_TREE` / `--work-tree` / `-c core.worktree=<abs>` class the tree they name as well as the cwd (the most dangerous class wins, so a main tree is `s3-main-tree`). The variables may come from a prefix assignment, `env`, or an earlier `export` / `declare` / `readonly` / bare assignment on the line. Anything else under `GIT_*` (except `GIT_AUTHOR_*`, `GIT_COMMITTER_*`, `GIT_TERMINAL_PROMPT`), a relative `core.worktree`, a non-literal value, or an earlier `eval` / `source` / undetermined `export` operand is unresolved: `?`.
  4. **Symlinks are resolved before a path or repository is classed.** A literal PATH target is classed by its spelling AND by the canonical path of its longest existing prefix; system / protected-config on either wins, a disagreement between the two (or no canonicalisable prefix) is `undetermined`. `class_of_repo` classes the realpath (the nearest existing ancestor canonicalised); system / protected-config on either spelling wins; `scratchpad` needs both the spelling and the realpath to be a scratchpad (`$TMPDIR` is canonicalised too).
  5. **`git -C<path>` / `-c<k=v>` attached forms** are not git syntax: the key keeps `-C` / `-c` and `?`. Option values are never in the verb: the git global options with values are split off before the subcommand is read, and for other tools a pre-subcommand operand shaped like a path / `k=v` (contains `/ = . : ~`) is an option value or a program file, never a subcommand (`gh -R o/r pr merge` -> `gh pr merge -R`).
  6. **Directly executed scripts** (`./run.sh`, `/abs/run.sh`) add the content hash when the file starts with `#!` (a binary is named by its basename alone); unreadable, missing, too large -> `?`. A relative path is resolved against a literal `cd` directory or the hook cwd; a leading `./` is dropped, other `.` / `..` components are `?`. The same resolution now applies to interpreter script files.
  7. **TOCTOU.** Beyond the heredoc rule: every command that is not shown to be a pure reader (a small allow-list: `cd echo printf cat grep ls chmod touch mkdir ...`) and is not a Table B command with extracted targets is an "unknown writer" (`rsync`, `curl -o`, `wget -O`, `patch`, any git write such as `checkout -- <path>`, an interpreter or wrapper run, an unresolved command word). After one, every program file of the line adds `?`; a directory written earlier covers the files below it. When a command runs concurrently with an unknown writer anywhere in the line (`&`, a pipeline stage), no program file of the line is pinned.
  8. **More interpreters.** `awk -f` / `awk '<prog>'` (body hash), `make -f`, `deno run <file>` / `deno eval`, `bun <file>` / `bun run <file>`, `osascript -e` / file, `php -r|-f|<file>`, `uv run <x.py>`, `tsx` / `ts-node`, `lua -e|<file>` get the file hash / body hash; a URL, a package script, `uv run <command>`, `bun x`, `deno task`, `lua -` are `?`; `cargo run` is always `?` (the program is the package's sources); a wrapper in front of any of these is `?`. Plain `make <target>` (default Makefile) is not covered (no operand names the program).
  9. **`git push` refspecs.** A `+refspec` operand is `--force`, a `:refspec` operand is `--delete` in the verb (same as the flags).
  10. **Option values are not in the key** (see 5; 44 real records carried an absolute path or `-c k=v`).
- Rulings of 2026-11-28 (fifth round; verifier round 4, tests `crates/blastguard/tests/verifier4_ask_learning_20261128.rs`). **The shape schema tag is `s4-`** (was `s3-`): the meaning of a shape changed, so every table learned with `s3-` keys is inert (a key of another schema is never matched; no migration).
  1. **A tool that is not in `askrules::MODELLED_TOOLS` adds `?`** to its verb (the single table; `shape.rs` refers to it, it is not copied). Every wrapper (`timeout`, `env`, `nohup`, `nice`, `command`, `exec`, ..), a directly executed script (`./run.sh`) and `fish` / `pwsh` file runs are therefore never Allow-learnable: their key does not pin the content they run.
  2. **`sh -c <body>` where the body is a single word or its first word contains `/`** (`sh -c ./run.sh`) adds `?`: the body hash is of the text, not of the script it names.

## Components

### 1. harness-core `asklearn` (pure, shared)
- `TargetClass`: `Worktree` (STRICTLY inside THE linked-worktree checkout the session's cwd belongs to (round 10): another session's linked worktree, however it is spelled or placed, is NOT `Worktree` (it is `Home` / `Undetermined`, never Allow-eligible), and with a cwd that is not in a linked worktree no path is `Worktree`. A linked worktree is decided as git decides (`repo_kind`/`gitlink_kind`): the `.git` file's `gitdir:` names an existing directory that holds a `commondir` resolving to an existing git dir and a `gitdir` back-link naming that `.git` file; a git dir without `commondir` (a submodule's `.git/modules/..`, whatever the submodule path is called) is a main tree; an unreadable or unresolvable gitlink (including a forged `.git` file whose text merely contains `/worktrees/`) is `Unknown` = `Undetermined`. Never a substring of the spelling. A checkout's root itself and the storage container (`.harness-worktrees/`, `~/.condukt/worktrees/<run>`) are NOT `Worktree`; the same holds for `~/.condukt/worktrees/<run>/<task>` -- only inside an individual checkout; being under the payload cwd places nothing by itself: a cwd that is a main tree makes its paths `MainTree`, a cwd that is no git tree (`~/.claude`, `~/src`, `$HOME`) gives no class and its paths fall to `Home` / `Undetermined`, and an unreadable git state is `Undetermined`. `shape::class_of_path` and `class_of_repo` agree on which checkout is `Worktree` (only the cwd's own); they differ in one respect: a path target must be strictly inside the checkout, while `class_of_repo` classes the REPOSITORY a git write acts on, which is the cwd's checkout root itself or any path in it), `Scratchpad` (under `/private/tmp/claude-*/…/scratchpad` or `$TMPDIR` session scratch), `ProtectedConfig`, `System` (SYSTEM_PREFIXES), `MainTree` (main working tree of a git repo; never Allow), `Home` (other paths under $HOME), `Undetermined` (target not statically resolvable), `None` (command has no write target).
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

## Rule files and the judging module (user ruling 2026-11-28, fifth round)

User rulings (verbatim): 「すすめて。たぶんコードだけでまとめるのは集中できなくなるのでよくない。あと万能はめざさず簡単になるとか失敗しつづけるを避けられるなら十分」 and 「すべてをデータにする必要もない。コードをつかうことも間違ってはいない。ただし処理するコードはまとまった形のlibとかにしてほしい」. Goal: make Allow/Deny rules easy to read and change; NOT a universal rule language.

Split: shape EXTRACTION (shell parsing, cwd/env tracking, repo class, hashing: `src/shape.rs` and the detectors) stays code. The JUDGMENT from a shape's facts moves into ONE module, `crates/blastguard/src/askrules/` (`mod.rs` public entry, `rules.rs` TOML schema + matcher, `builtin.rs` the invariants below). It lives in blastguard (not harness-core) because only the blastguard verdict side judges; overwatch (a dependency of blastguard) keeps writing the table through harness-core `asklearn` and never judges. `learned.rs::apply` and the scattered checks (`Shape::allow_permitted`, never-allowed class, `?` handling, deny-beats-allow in `asklearn::lookup`) are routed through this module and removed from their old places. Single entry, pure (no I/O, no env):

```rust
pub struct Facts { /* rule, class, system_harm, verbs: Vec<Verb{tool, sub, flags, program}> */ }
impl Facts { pub fn from_shape(shape: &harness_core::asklearn::Shape, system_harm: bool) -> Facts }
pub struct RuleSet;                                   // parsed rule file; never fails to construct
impl RuleSet { pub fn from_toml(text: &str) -> RuleSet; pub fn warnings(&self) -> &[String]; pub fn allow_enabled(&self) -> bool }
pub enum Judgement { Allow(String /*reason*/), Deny(String), NoMatch }
pub fn judge(facts: &Facts, sets: &[&RuleSet]) -> Judgement
```
The learned table (`table.json`) is converted to a `RuleSet` (one exact-shape rule per entry) so there is one matcher. Reading files (env, HOME, stderr) is the binary's job; `judge` only sees values. Only things that get simpler as data are data (the human allow/deny list, the learned table); any complex condition is code inside the module.

### Files
- Human rules: bundled `crates/blastguard/rules/ask-rules.toml` (compiled in with `include_str!`). Test override: env `BLASTGUARD_ASK_RULES=<path>` replaces it (a path that does not exist, cannot be read or is not TOML is undetermined: NO Allow at all -- learned ones included -- is honoured, the Ask stays an Ask).
- Learned: existing `~/.blastguard/learned/` (`table.json`, `BLASTGUARD_LEARNED_TABLE`).

### Format
```toml
[[rule]]
decision = "allow"            # or "deny"
reason   = "why (required, non-empty)"
[rule.match]                  # at least one key; all given keys must hold
rule       = "git-whole-tree-stage"   # detector rule id
class      = ["worktree", "scratchpad"] # target class (any of); labels without the `s3-` tag
tool       = "git"                    # verb basename
sub        = "add"                    # subcommand
flags_all  = ["-A"]                   # every listed normalised option present
flags_any  = ["-f", "--force"]        # at least one present
flags_none = ["--force"]              # none present
program    = "#26610971178c87de"      # the `#<fnv1a64>` program hash of the verb
```
Only structured facts of the shape; exact string equality; NO regex, NO raw command text. `tool/sub/flags*/program` constrain a verb. A shape with several verbs: a Deny rule matches if ANY verb satisfies it; an Allow rule matches only if EVERY verb satisfies it (narrow for Allow, broad for Deny).

### Evaluation
As implemented in `askrules::judge`: 1. Any matching deny rule (human or learned, any file; a deny needs no eligibility) -> Deny. 2. Poison check: a set whose denials are unknown (unreadable / unparsable file, unreadable restrictive rule) -> `NoMatch`. 3. Positive allow-list eligibility and the invariants below (`builtin::allow_eligible`) -> else `NoMatch`. 4. A matching `not_learnable` entry -> `NoMatch`. 5. Any matching allow rule (no `except` entry matching) -> Allow. 6. Else `NoMatch` -> the Ask stands. Deny beats allow across files too. A Deny is never produced for a call the detector already denied, and a detector Deny is never downgraded (unchanged).

### Invariants fixed in code (`builtin.rs`; no file can change them)
- (a) class `system`, `protected-config`, `main-tree` never match an Allow rule; `system_harm` never matches one.
- (b) a shape whose verbs contain `?` (`UNRESOLVED_VERB`) never matches an Allow rule.
- (c) an unknown key (top level, `[[rule]]`, or `match`), a wrong type, an unknown `decision`, a missing/empty `reason`, an empty `match`, an unparsable file, or an unreadable existing file disables ALL Allow rules of THAT file (`allow_enabled() == false`; warning in `warnings()`, printed to stderr by the binary with the reason). Deny rules of the file stay active when they themselves parse (rule-by-rule); if the whole file is unparsable no rule is usable. An unreadable or unparsable file, an unknown top-level key, or a deny / `not_learnable` rule that could not be read additionally disables EVERY Allow (learned ones included; `poisons_allow`): what the file would have denied is unknown. The bundled file is compiled in; an override path that does not exist counts the same.
- (d) the human rules file path (`crates/blastguard/rules/**`, and the `BLASTGUARD_ASK_RULES` target) is protected config like `.blastguard/learned/**`: Write/Edit/shell writes are never Allow, and neither is removing or renaming an ANCESTOR directory of the override file (`rm -r`, `mv`).

### Positive allow-list: only fully modelled shapes can be Allowed (coordinator ruling 2026-11-28, after four failed verifier rounds)

Closing reach holes one wrapper at a time failed four times (r4: `env -C`, `env -S`, `bash -c`, `find -exec`, `./run.sh` through a wrapper, `sh -c ./run.sh`, fish/pwsh files). The invariants (a)(b) above are therefore replaced as the Allow gate by an inverted, positive rule, defined ONCE in `askrules/builtin.rs` (`MODELLED_TOOLS`, one table):

- A shape can match an Allow rule (human or learned) only if ALL hold: (1) its class is one of `none`, `worktree`, `scratchpad` (so `home` and `undetermined` are no longer Allow-eligible either); (2) EVERY verb's tool (first token) is in `MODELLED_TOOLS` with its modelled subcommands: `git` with a listed local subcommand (`GIT_SUBCOMMANDS`: add am apply bisect blame branch checkout cherry-pick clean commit describe diff grep log ls-files merge mv notes rebase reflog restore rev-parse revert reset rm show stash status switch tag -- it includes destructive local ones such as `reset` / `clean` / `rm`; what limits them is the class of the repository they act on; `worktree` is NOT listed because `worktree remove|move|prune` delete or move another checkout) and none of the options in `GIT_REFUSED_FLAGS` (`-C -c --exec-path --config-env --git-dir --work-tree`: they change which repository / configuration / helper the command acts on, so such a line is never Allow-learnable or rule-Allowed), `rm`, and `bash`/`sh`, which are listed but never eligible (next bullet). An interpreter run (`python3`, `python`, `node`, `perl`, `ruby`) is NEVER eligible (ruling 2026-11-28: no attempt at completeness, fail closed). An environment assignment before a command word (`PATH=`, `HOME=`, `EDITOR=`, any name) or a standalone assignment earlier on the line adds `?` to the verbs; (3) no verb contains `?`; (4) not `system_harm`.
- `bash` / `sh` are listed but NEVER eligible (round 6: a script file's content may change after it was learned; `ShellFile => false` in the implementation).
- Everything else is automatically not Allow-eligible: unknown tools, every wrapper that runs or `cd`s another command (`env`, `sudo`, `timeout`, `nice`, `nohup`, `exec`, `command`, `xargs`, `find -exec`, `sh -c` / `bash -c` bodies) unless the lib models the wrapped command completely (initially: none are), unknown interpreters (`fish`, `pwsh`, ...). A Deny rule may still match them.
- This is the single place to extend: adding a tool to the table is the only way a new kind of shape becomes Allow-eligible. Invariants (a)(b) stay as redundant checks.
- Intended to turn the 7 tests of `verifier4_ask_learning_20261128.rs` GREEN; they should be read against this rule.

### Lines an Allow can apply to (main-session ruling 2026-11-28, after six verifier rounds)

Each round found a new spelling that rides a learned Allow, so the Allow (learned and rule) is limited STRUCTURALLY to lines meeting ALL of the following; any one failing leaves the Ask standing. Deny rules are judged as before, whatever the line. This is in addition to the positive allow-list above.

1. **One simple command.** The whole line is a single simple command: no pipeline, `;`, `&&`, `||`, `&`, `!`, `time`, `for` / `while` / `case` / `if`, function definition, subshell, brace group, command substitution, process substitution, heredoc or here-string. NO redirect of any kind (round 9 narrowing, user ruling "do not aim at completeness"): a brush AST with even one redirect is not eligible.
2. **Every word is a static literal.** The command word and every argument contain no variable or parameter expansion, `$'..'`, brace expansion, glob, arithmetic expansion, tilde expansion or backslash escape, and no assignment stands before or among the words. A word that is only quoted (`'..'`, `".."`) is a literal and is judged by its unquoted value. Decided by ONE function on the brush AST (`detect/ast/static_simple.rs`, `is_single_static_command`), never on the command text.
3. **Path arguments are the real path.** Each path that decides the class (a redirect target, a file operand, the git repository) has no `..` component, and the canonical path of its longest existing prefix equals its spelling byte for byte: no symlink on the way, and a case difference (macOS) counts as a difference. A path failing this is classed `undetermined` (`shape::class_of_path`, `class_of_repo`), which is never Allow-eligible.

4. **The command word is a bare name.** A command word containing `/` (`./git`, `<dir>/git`) is not eligible: the shape key keeps only its basename, yet the shell runs that file (a shebang-less executable runs as a shell script). A word starting with `=` (zsh `=word`, expanded to a command path) is not a static word, in any position (`is_static_word`).
5. **No symlink on the way.** Every component of a path that decides the class is `lstat`ed; a symlink component, whether or not its target exists (a dangling link is written THROUGH), makes the class `undetermined`. An `lstat` failure other than not-found is also `undetermined`.

6. **No unquoted shell operator character.** Judged on the AST AND the raw text: a simple command is eligible only if the AST says so and the raw line has none of `<` `>` `!` `|` `&` `;` outside `'..'` / `".."` (`is_single_static_command`, `detect/ast/static_simple.rs`). zsh (the Bash tool's shell) reads `<->` as a numeric glob and `>!` / `>>!` / `&>!` / `2>!` as clobber redirects where brush reads `> !` plus an argument. A target `!` is also `Undetermined` in the shape facts.
7. **Placed by the tree kind, not by the cwd.** A path under the payload cwd is classed by the kind of git tree the cwd is (see Components 1), never `Worktree` merely for being under the cwd.
8. **Destination-writing tools are out of scope.** `cp` / `mv` / `install` / `ln` / `rsync` / `ditto` / `mkdir` / `touch` are not in `MODELLED_TOOLS` (verb `?`), and a shape with one of them whose class would be `none` / `scratchpad` / `worktree` is classed `undetermined` (`shape::shape_of`). Where they write depends on the destination's kind (`cp f DIR` writes `DIR/<f>` through a symlink child; a relative DIR was judged against the hook process cwd), so it is not pinned. Never Allow-eligible. (Replaces the round-8 "directory destination is undetermined" rule.)

A shell call with no readable command text is never eligible (a file tool, which has no shell line, is). A shape key does not carry this structure, so a learned entry can never be shown to apply to a line of another structure: the gate is evaluated on each call's own text.

Rules-file protection (invariant (d)): the override file and its ancestor directories are compared after canonicalising the longest existing prefix of the operand (and of the override path) and case-insensitively; a `cp` / `mv` whose destination is, or may be, a directory is judged on the file it creates there (`DEST/<basename SRC>`).

### "Allow, except ..." and regex on the restrictive side only (user ruling 2026-11-28, verbatim: 「allowだが、これはdenyするを書ければ問題は軽量になる。そこを正規表現までふくめればもっとカバレージできる。問題が複雑になっているとは思うので、コードだけでなんとかするとかはしない方がいい気がするし、それは判断して適用して」)

The positive allow-list stays; fine-grained carve-outs are added as data, not as per-case code.

```toml
[[rule]]                      # allow, with exceptions
decision = "allow"
reason   = "stage in a worktree"
[rule.match]
tool = "git"
sub  = "add"
except = [                    # each entry: all its keys hold -> this rule does NOT match
  { command_regex = '\s-f\b' },
  { class = ["scratchpad"], verbs_regex = 'push' },
]

[[deny]]                      # top-level deny (same as decision = "deny")
reason = "wrapper that cd's elsewhere"
[deny.match]
command_regex = '\benv\s+-[^ ]*[CS]'

[[not_learnable]]             # the shape/command never matches ANY allow (human or learned); Ask stands; not a Deny
reason = "inline shell"
[not_learnable.match]
command_regex = '\b(ba|z|fi)?sh\s+-c\b'
```
- Conditions (`match` of deny / `[[deny]]` / `not_learnable`, and each `except` entry) take the structured keys of the base format plus `rule_regex`, `class_regex`, `verbs_regex` (the joined verbs string `a b,c d`) and `command_regex` (searched in the raw command string AND in the quote-stripped words the shell passes, `git ad''d -A` = `git add -A`; either matching is a match, words that cannot be determined count as matched; `Facts::with_command(&str)`; the binary passes the Bash command, other tools none). Regex = Rust `regex` crate syntax, UNANCHORED search. `regex` is already a workspace dependency (`Cargo.toml`: `regex = "1"`, in `Cargo.lock`, used by harness-core); blastguard would add `regex = { workspace = true }` -- proposed, to be confirmed at implementation.
- **No regex on the permissive side.** A `match` of an `allow` rule may not contain any `*_regex`; one that does disables ALL Allow rules of that file (same treatment and stderr warning as invariant (c)). Reason: a regex is a guess, so it may never create or widen an Allow; on the restrictive side a false positive only fails safe.
- Fail-closed on the restrictive side: a regex that does not compile, or whose value is not a string, counts as MATCHED (a broken `except` blocks its allow rule; a broken `not_learnable` blocks allow; a broken `[[deny]]` regex denies, as ruled -- a visible consequence: one typo denies every Ask until fixed), and the reason is printed to stderr. A `command_regex` condition evaluated on `Facts` that carry no command also counts as MATCHED.
- Evaluation order (as implemented in `askrules::judge`): `[[deny]]`/deny rules (any file) -> poison check (an unreadable/unparsable file or restrictive rule allows nothing) -> positive allow-list eligibility -> `not_learnable` -> allow rules whose `except` list has no matching entry -> `NoMatch`. The invariants (a)(b) are redundant checks inside the eligibility step. Learned-table entries behave as allow/deny rules with no regex.
- First contents of the bundled `crates/blastguard/rules/ask-rules.toml` (proposal; the reach paths found by verifier rounds r1-r4, as `not_learnable` / `deny` regexes instead of code): `env\s+-[^ ]*[CS]` (env -C / -S), `\bfind\b.*-exec`, `\b(ba|z|fi)?sh\s+-c\b`, `\bpwsh\b`, a wrapper in front of `./run.sh`, and fish/pwsh script files. Each entry carries its reason and the round that found it. The positive allow-list is still the primary defence; these entries are the cheap, visible, editable second layer.
