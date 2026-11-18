# `detect::ast` — supported subset and EFFECT × TARGET judgement

Task S1 `subset-spec` (2026-11-17). Specification only: the frozen types are in
`judge.rs` (`Effect`, `PathClass`, `Finding`, `Judgement`, `PathCtx`, `judge`,
`classify`); nothing here is wired into `detect` yet and no verdict changes in S1.
(Since S3b, `judge` is wired for the B.1 rows only — see the Stage map.)

## Binding user rulings (2026-11-17)

1. brush-parser with an explicit SUPPORTED SUBSET allowlist of node and expansion
   kinds (Table A). Anything outside → `Undetermined(Cause::Unsupported)` → main
   Ask / subagent Deny. No per-corner-case bash modelling.
2. Judge by EFFECT × TARGET PATH via rule tables (Tables B, C), not by a
   command-name denylist. Unresolvable target → restrictive. Unknown program (no
   effect model) → Allow. Undetermined command word → Undetermined.
3. System-harm targets are always Deny.
4. perl / ruby / node inline programs (`-e` etc.) are OUT of the subset → Ask.
   Python gets a real parser later (S5).
5. `rm -r` (and other deletes) whose target statically resolves inside the session
   scratchpad or own worktree → Allow; unresolved or outside → unchanged (Ask/Deny).
6. AST analysis Undetermined → Ask; the old string harm detection is kept ONLY
   raise-only (may lift Ask → Deny, never lower to Allow).
7. Migrated detectors drop the old string-reading fallback (S6).

Follow-up rulings (2026-11-17, on the S1 open questions):

8. `Scratch` = every temp and cache deletion root: `/tmp`, `/private/tmp`,
   `$TMPDIR` and the cache roots `rm -r` already allows
   (`SafeRoots::classify_disposable`). Not `classify_temp` alone.
9. `OwnWorktree` = only the git checkout that contains the session cwd. Other
   worktrees keep today's verdict. **Correction (user ruling 2026-11-18):**
   today's verdict there is NOT a confined Ask for a recursive `rm`: a
   recursive `rm` whose operands resolve strictly inside a worktree storage
   root stays **Allow** even when the session cwd is outside that checkout
   (the `scope.rs` / `rm.rs` worktree-storage Allow, user ruling 2026-09-30,
   backlog 873651b9, wins). The earlier parenthetical "(confined Ask)" was
   wrong for that row.
10. A program whose text is not in the command (`bash script.sh`) is
    Undetermined → Ask (subagent Deny): an interpreter running a file is an
    exception to "unknown program → Allow". When the same command writes that
    file, the written body is judged recursively instead (S4).
11. The remaining open questions stay legacy (today's behaviour) and are
    revisited at S6; see "Open questions".

S2 rulings (user ruling 2026-11-17, spec change made before the S2 tests):

12. `$HOME` / `${HOME}` and `~` are Known values: the `HOME` of the hook's own
    environment (Table A.3).
13. `.` / `source` of a file stay **Ask** (main) / Deny (subagent) until a
    quarantine plugin exists (Table B.5); S4 does not judge the sourced body.

S2 supplement rulings (user ruling 2026-11-17, after the S2 RED verification):

14. The legacy string layer stays in full, **raise-only**, until S6 — for
    every row, not only the harm readings. A Table C Allow that a legacy
    reading answers Ask / Deny today (`dd of=`, `truncate`, `find -delete` on
    Scratch) stays at the legacy verdict until S6 retires that reading.
    **ruling 14 replaced per rule (2026-11-18): rm legacy reading retired
    when new layer is Known** (user ruling 2026-11-18: retire the legacy
    layer rule by rule). For `rm`, when every target of the command is
    statically resolved and lands in a Table C Allow class (Scratch /
    OwnWorktree), the legacy `rm` reading (`detect/rm.rs` `analyze_rm`) no
    longer raises it: `detect/rm/new_layer.rs`. Scratch included by user
    ruling 2026-11-18 ("仕様どおり Allow": a quoted in-scratch literal gets
    its unquoted spelling's verdict). Every other `rm` (an unmodelled
    option, an Undetermined value, a `legacy` / Unresolved / System /
    Protected / Workflow class) keeps the legacy reading as before. Other
    legacy readings of the same command line still apply.
15. Python keeps the legacy readings until S5 (the Python parser).
16. `.` / `source` become Ask (main) / Deny (subagent) **together with the
    quarantine plugin**, not in S4. Until then their verdict is legacy
    (today's). This refines ruling 13's timing.
17. Direct execution of a program file (`./run.sh`, `scripts/foo.sh`, an
    absolute path) whose body is not in the command text is Undetermined →
    Ask (main) / Deny (subagent), like ruling 10's `bash script.sh` (S4).

Vocabulary: **Undetermined** = `Judgement::Undetermined(cause)`, resolved to
Ask (main) / Deny (subagent, `interactive::ask_available`). **Unresolved** =
`PathClass::Unresolved`, one target that could not be placed (Table C row).
**legacy** in a cell = S3 must reproduce today's verdict of the existing rule
named there, pinned by its existing tests; the ruling does not change it.

## Table A — supported node / expansion kinds

Everything not listed as supported is `Unsupported`. "Value Undetermined" means the
script is still `Known`: the word's `Value` is `Undetermined` and feeds Table C as
`Unresolved` (or, for a command word, makes the judgement Undetermined).

### A.1 Parse stage (`parse.rs`)

| Input | Result |
|---|---|
| > `MAX_PARSE_BYTES` (8 KiB) | `TooLong` → Deny (ruling 2026-11-16) |
| nesting > `MAX_PARSE_DEPTH` (16), over-counting pre-scan | `TooDeep` → Deny (ruling 2026-11-16) |
| zsh-only syntax (`${(flags)…}`, `=(cmd)`, …) | `ZshOnly` → Undetermined |
| brush-parser error / unterminated heredoc | `Parse` → Undetermined |
| panic in brush-parser or the evaluator | `Panic` → Undetermined |

### A.2 Commands and lists (`eval.rs`)

| Node (`brush_parser::ast`) | Status |
|---|---|
| `Program`, `CompoundList`, separators `;` and `&` (`Context::Background`) | supported |
| `AndOr` `&&` / `||` (both arms emitted, states joined) | supported |
| `Pipeline` incl. `!` (stages get `Context::Pipeline`, stdin piped) | supported |
| `Command::Simple` (prefix assignments, argv, redirects) | supported |
| `CompoundCommand::BraceGroup`, `Subshell` | supported |
| `IfClause`, `CaseClause`, `WhileClause`, `UntilClause` (fixpoint) | supported |
| `ForClause` (literal list ≤ 256 unrolled; else body once, var Undetermined) | supported |
| `Arithmetic` `(( ))`, `ArithmeticForClause`, `let` | supported only when every name read is bound to a literal on the line (`arith.rs`); else `Unsupported` |
| `Command::ExtendedTest` `[[ ]]` | supported (operands evaluated; arithmetic operators as above) |
| `Command::Function` (definition) | **Unsupported** |
| `CompoundCommand::Coprocess` | **Unsupported** |
| work budget (`MAX_WORK` 100 000) / `MAX_COMMANDS` (10 000) exceeded | **Unsupported** |

### A.3 Word pieces (`eval.rs::pieces`, `eval/words.rs`)

| Piece | Status |
|---|---|
| `Text`, `SingleQuotedText`, `DoubleQuotedSequence`, `EscapeSequence` | supported (literal) |
| `AnsiCQuotedText` `$'…'` | supported if decoded; else value Undetermined |
| `GettextDoubleQuotedSequence` `$"…"` | value Undetermined |
| `TildeExpansion` `~` (also after `=` / `:`) | value Known: the hook environment's `HOME` (user ruling 2026-11-17, ruling 12); `~user` is not covered by the ruling → value Undetermined. **Implemented (S3) only for a bare `~` that starts the word**; `~` after `=` / `:` inside a word is still Undetermined (restrictive; not yet implemented) |
| `$HOME` / `${HOME}` | value Known: the hook environment's `HOME` (user ruling 2026-11-17, ruling 12), unless `HOME` is assigned on the line (then the named rule below) |
| `$x` / `${x}` named | literal iff bound to a literal on the line; else value Undetermined |
| positional / special parameters (`$1`, `$@`, `$?`, …) | value Undetermined |
| other parameter operations (`${x:-v}`, `${#x}`, `${x/a/b}`, …) | value Undetermined; **Unsupported** if it holds `$( )` / backquote or reads an undetermined name in arithmetic |
| `CommandSubstitution` `$( )`, backquotes | body evaluated as a nested script (`Context::CommandSubstitution`); value Undetermined |
| `ArithmeticExpression` `$(( ))` | value Undetermined; **Unsupported** under the A.2 arithmetic rule |
| unquoted expansion subject to splitting / globbing, unquoted glob / brace | value Undetermined |
| process substitution `<( )` / `>( )` | body evaluated (`Context::ProcessSubstitution`) |

### A.4 Redirections

| Redirect | Status |
|---|---|
| `<` `>` `>>` `>|` `<>` `<&` `>&` (`RedirOp::*`), `&>` `&>>` | supported; target `RedirTarget::Path` / `Fd` |
| heredoc `<<` / `<<-` (quoted, or unquoted without expansions → literal body) | supported; body Undetermined if unquoted with expansions |
| here-string `<<<` | supported |
| negative fd | **Unsupported** |

### A.5 Builtins with state effects (evaluator, not Table B)

`cd` (literal, `&&`-tied) moves the tracked cwd; `cd -` / bare / options / `pushd`
/ `popd` / `..` → cwd Undetermined. `eval`, `source`, `.`, `trap`, `alias`,
`shopt`, `builtin`, `command`, an undetermined command word, `declare`-family
options, non-plain `set` make the state opaque (later values Undetermined).
For **judgement**, `eval` is Undetermined (it runs code not in the script) until
S4 models it; `source` / `.` become Ask (main) / Deny (subagent) together with
the quarantine plugin and are legacy until then (rulings 13, 16; B.5).

## Table B — effect × target extraction

Common rules for every row:

* Option parsing is per command; an option not in the row's list, or an
  Undetermined option word, → `Judgement::Undetermined(Unsupported("<cmd> option
  <o> not modelled"))`. `--` ends options.
* A target word with an Undetermined value → `PathClass::Unresolved`.
* A relative target: resolved against `SimpleCommand::cwd` exactly as
  `RedirTarget::Path` / `eval::resolve_at` (literal as written when no `cd`
  applies → placed against `PathCtx::hook_cwd`; Undetermined after an
  undetermined `cd`).
* A command not in this table (and not a wrapper / interpreter) has no effect
  model → no finding (Allow); its redirects are still judged (B.2). An
  interpreter running a program file is NOT such a command (ruling 10, B.5).
* Undetermined command word (`argv[0]`) → `Judgement::Undetermined`.

### B.1 File commands

| Command | Effect | Targets | Notes |
|---|---|---|---|
| `rm` | `Delete{recursive: -r/-R/--recursive}` | every operand | `-f -i -I -v -d` modelled |
| `rmdir`, `unlink` | `Delete{recursive:false}` | every operand | `rmdir -p`: every ancestor component too |
| `mv` | sources `Delete{recursive:true}`; destination `Truncate` | sources = all operands but the last (all with `-t DIR`); dest = last / `-t DIR` | `-f -i -n -v -T -t` |
| `cp` | destination `Truncate`; with `-r/-R/-a` also `CreateTree` | last operand / `-t DIR` | sources are reads (no finding) |
| `install` | destination `Truncate` | last operand / `-t DIR`; `-d`: every operand `CreateTree` | `-m -o -g` take an argument |
| `ln` | `Link` | link name = last operand / `-t DIR` (one operand: basename in cwd) | link target is not a target; `-s -f -n -v -T -t` |
| `chmod`, `chown`, `chgrp` | `Metadata` (recursive with `-R`) | operands after the mode / owner spec | `--reference=F` reads F |
| `sed -i` / `--in-place` | `Rewrite` | input files (script operand dropped unless `-e`/`-f` given) | BSD `-i ''` / `-i.bak` suffix; a script containing `w`/`W`/`e` or `s///w`/`s///e`: legacy until S6 (Open question 2) |
| `perl -i` | — | — | **out of subset**: perl inline program → Undetermined (ruling 4) |
| `tee` | `Truncate` (`Append` with `-a`) | every operand | `-a -i -p` |
| `truncate` | `Truncate` | every operand | `-s` takes an argument; `-r RFILE` reads |
| `dd` | `of=F`: `Truncate` (`Rewrite` with `conv=notrunc`) | `F` | `if=` is a read; any other operand form not `k=v` → Unsupported |
| `find` | `-delete`: `Delete{recursive:true}` on each search root (descendants); `-fprint* F` / `-fls F`: `Truncate F` | search roots = words before the first expression word (`-H -L -P -D x -O*` skipped) | `-exec/-execdir/-ok/-okdir CMD … {} ;|+`: legacy (`find_xargs.rs`) until S6 (Open question 1) |
| `xargs` | the wrapped command's effect | wrapped command's own literal operands + the stdin-supplied operands → `Unresolved` unless stdin is `StdinSource::Literal` | `-0 -n N -I R -L N -P N -d D -r -t` |

### B.2 Redirections (the shell's effect, judged for every command)

| Redirect | Effect | Target |
|---|---|---|
| `>`, `>|`, `&>`, `N>` | `Truncate` | `RedirTarget::Path` value |
| `>>`, `&>>`, `N>>` | `Append` | `RedirTarget::Path` value |
| `<>` | `Rewrite` | `RedirTarget::Path` value |
| `<`, `<<`, `<<<`, `<&`, `>&N`, process substitution | no effect | — (`/dev/null` etc. are `Other`) |
| `>&word` with a non-fd word | `Truncate` | the word, as a path |
| compound-command redirects (`{ …; } > f`, `done > f`) | as above | the extra empty-argv `SimpleCommand` |

### B.3 Wrappers (unwrap, then judge the wrapped command by its own row)

| Wrapper | Skipped before the wrapped command | Notes |
|---|---|---|
| `env` | options `-i -u NAME -0 -C DIR`, then `NAME=V` words | `-S` (split string) → Unsupported; `-C DIR` sets the wrapped cwd |
| `sudo`, `doas` | options; `-u/-g/-C/-D/-h/-p/-r/-t/-U USER` take an argument | `sudo -s/-i` with no command, `sudo -e` (sudoedit) → Unsupported |
| `nice` (`-n N`), `ionice` (`-c -n -t`), `nohup`, `time` (`-p`), `stdbuf` (`-i -o -e`), `setsid`, `caffeinate` (`-d -i -m -s -u -t N -w PID`), `chrt`, `taskset` (mask/list arg), `timeout` (`DURATION`, `-s SIG`, `-k D`, `--foreground`, `--preserve-status`), `unbuffer`, `arch` (`-arch A`, `-x86_64`, `-arm64`), `flock` (lock file, `-c CMD` → Unsupported) | as listed | the flock lock file is created (`Truncate`-free create: no finding) |
| `command`, `builtin` | as `eval::unwrap_wrappers` (`-p`, `--`); `command -v/-V` = lookup, no effect | |
| `exec` | `-a NAME -c -l` | `exec` with redirects only: B.2 applies to the shell |
| `su` | — | `su -c` / any `su` → Unsupported |
| `xargs` | see B.1 | |

### B.4 git (path effects only)

`git -C DIR` / `--work-tree=DIR` set the base; `--git-dir`, `-c k=v` are skipped;
an Undetermined value → `Unresolved`. Target "work tree" = the base (`-C` DIR,
else the command's cwd).

| Subcommand | Effect | Target |
|---|---|---|
| `reset --hard` (also `--merge`, `--keep`) | `GitDiscard` | the work tree |
| `clean` (`-f`; `-d` → recursive; `-x/-X` ignored files too) | `Delete{recursive: -d}` | each pathspec, else the work tree (search-root semantics: `IsRoot` acceptable) |
| `checkout -- <paths>` / `checkout .` / `checkout <tree-ish> -- <paths>` | `GitDiscard` | each pathspec |
| `restore [--worktree] <paths>` (default worktree); `--staged` only | `GitDiscard`; staged-only: no effect | each pathspec |
| `add -A` / `--all` / `add .` / `add -u` | `GitStage` | each pathspec, else the work tree |
| other subcommands | no path effect here | non-path git rules (push, branch -D, hooksPath, …) stay in `detect/git.rs` (legacy, outside this table) |

### B.5 Interpreters (`program_source`)

| Command | Program source | Judgement |
|---|---|---|
| `bash` / `sh` / `zsh` / `dash` / `ksh` `-c PROG` | `InlineLiteral` | PROG analysed and judged recursively (S4); Undetermined PROG → Undetermined |
| same, stdin (`bash`, `-s`, `-`, `/dev/stdin`) | `Stdin(StdinSource)` | `Literal` body judged recursively (S4); `FromCommand` / `File` / `Fd` / `Undetermined` → Undetermined |
| same, script file (`bash x.sh`); likewise any interpreter running a program file (`python3 x.py`, `perl x.pl`, …) | `File(Value)` | **Undetermined** → Ask / Deny (ruling 10). Exception: when an earlier command of the same script writes that file (matched by resolved path) with a determined body (`cat > x.sh <<'EOF' … EOF`), that body is analysed and judged recursively (S4) |
| `eval` | `Undetermined` | Undetermined (S4) |
| direct execution of a program file (`./run.sh`, `scripts/foo.sh`, `/abs/run.sh`) | `File(Value)` | **Undetermined** → Ask / Deny (ruling 17, S4), same exception as the script-file row |
| `source`, `.` (reading a file) | `File(Value)` | **Ask** (main) / Deny (subagent) together with the quarantine plugin; legacy until then (rulings 13, 16); not judged recursively in S4 |
| `python*` `-c` / stdin / file / `-m` | as `program_source` | legacy readings until S5 (ruling 15); from S5 the Python parser |
| `perl` / `ruby` `-e`/`-E`, `-i`, stdin, `node` `-e`/`-p`/stdin | — | **out of subset** → Undetermined (ruling 4) |
| wrapper whose operand is an interpreter or Undetermined | `Undetermined` | unwrap (B.3), then the interpreter's row |

## Table C — PathClass × Effect → verdict

`PathClass` is computed only from existing predicates (named on each variant in
`judge.rs`); first matching class wins in this row order. Main column / subagent
column. The subagent column is always the main column with Ask → Deny
(`interactive::ask_available`); no cell differs otherwise.

| PathClass (predicate) | Delete (rec / non-rec) | Truncate / Append / Rewrite | Metadata | Link | CreateTree | GitDiscard | GitStage |
|---|---|---|---|---|---|---|---|
| **System** (`harm_mark::harms_system` + `protected::real_system_path_block`; escapes via `Ctx::confined_root`) | Deny† / Deny† | Deny† / Deny† | Deny† / Deny† | Deny† / Deny† | Deny† / Deny† | Deny† / Deny† | Deny† / Deny† |
| **Protected** (`exclude::is_protected_path`, `protected::physical_landing`; lifted inside `classify_worktree_checkout` Inside) | Deny / Deny (`protected_disarm_deny`, `protected_tree_deny`) | Ask / Deny (`protected_path_block`) | Ask / Deny | Ask / Deny | Ask / Deny | legacy | legacy |
| **Workflow** (`exclude::is_workflow_path`) | Ask / Deny | Ask / Deny | Ask / Deny | Ask / Deny | Ask / Deny | Ask / Deny | Ask / Deny |
| **Persistence** (`launch_agents_new_file_ask`, new file only) | — | Ask / Deny | — | Ask / Deny | Ask / Deny | — | — |
| **Scratch** (`SafeRoots::classify_disposable` Inside: `/tmp`, `/private/tmp`, `$TMPDIR`, cache roots — ruling 8) | Allow / Allow (ruling 5) | Allow / Allow | Allow / Allow | Allow / Allow | Allow / Allow | Allow / Allow | Allow / Allow |
| **OwnWorktree** (the checkout containing the session cwd — ruling 9; see notes) | Allow / Allow (ruling 5) | Allow / Allow | Allow / Allow | Allow / Allow | Allow / Allow | legacy (`managed_worktree_git`) | Allow / Allow |
| **OtherWorktree** (`SafeRoots::classify_worktree` Inside, not OwnWorktree) | legacy (ruling 9 as corrected 2026-11-18: today's verdict — recursive `rm` strictly inside a worktree storage root is Allow) | legacy | legacy | legacy | legacy | legacy | legacy |
| **Project** (`SafeRoots::classify` Inside / IsRoot) | legacy (`deletion::judge_rm` recoverability; non-rec: legacy rm arm) | legacy | legacy (`chmod -R` confined Ask) | legacy | legacy (scratch-place: managed → unchanged) | legacy (`reversible`) | legacy (`git_stage`) |
| **Other** (resolved, `Placement::Outside`) | legacy (ruling 5: unchanged Ask/Deny) | legacy | legacy | legacy | Ask / Deny (scratch-place rule) | legacy | legacy |
| **Unresolved** | Ask / Deny | Ask / Deny | Ask / Deny | Ask / Deny | Ask / Deny | Ask / Deny | Ask / Deny |

† `Decision::deny_system_harm` (never downgraded on repeat). Ruling 3.

Notes:

* OwnWorktree is composed from existing predicates only: the target is
  `SafeRoots::classify_worktree_checkout` `Inside { root: R }` and the session
  cwd (`SafeRoots::session_cwd`) is `R` or inside `R` by the same predicate.
  Any `Undetermined` → not OwnWorktree.
* `IsRoot` of a Scratch / worktree storage root (`rm -rf /tmp`) is NOT Scratch /
  OwnWorktree; it falls through to Project / Other (today: `judge_rm` refuses the
  root of a temp / worktree area → Deny).
* A finding set is folded most-restrictive-first: any Deny → `Judgement::Deny`
  (system_harm if any System Deny); else any Ask → `Judgement::Ask`; else Allow.
* An Unresolved Delete in Table C is Ask/Deny, but ruling 5 says unresolved
  deletes stay "unchanged". Where today's rule answers Deny (e.g. recursive `rm`
  of an unplaceable operand), the raise-only legacy layer keeps that Deny.

## Undetermined handling (raise-only legacy layer)

1. `analyze(text)` = `Undetermined(cause)` or `judge(..)` = `Undetermined(cause)`
   → the verdict is **Ask** (main) / **Deny** (subagent), except `TooLong` /
   `TooDeep` → Deny (ruling 2026-11-16).
2. The legacy string harm detection (the readings in `detect.rs` /
   `readings.rs`) still runs on that path, but its result may only **raise**:
   legacy Deny lifts Ask → Deny (keeping `system_harm`); legacy Ask or Allow
   leaves the Ask. It is never consulted to lower a verdict to Allow.
3. When `judge` is Known (Allow / Ask / Deny), S3 keeps the legacy readings
   — all of them, not only the harm readings (ruling 14) — raise-only too
   until S6 retires them for the migrated rows; a migrated row's
   detector no longer has a string-reading fallback (ruling 7).
4. A `judge` panic is caught and is `Undetermined(Panic)` → rule 1.

## Stage map

| Stage | Implements |
|---|---|
| S1 `subset-spec` | this document; frozen types in `judge.rs` (stubs, not wired) |
| S2 `red-tests` | an independent author (not the S3 implementer; CLAUDE.md §2(a)) writes a RED test for each Table B row and each Table C cell that is not `legacy`, each observed failing against the S1 stub before S3 makes it GREEN |
| S3 `path-cutover` | Table A enforcement for judged commands; Table B B.1 (all rows except `perl -i` which is already Undetermined), B.2, B.3, B.4; Table C (all rows); `classify` from the existing predicates; wire `judge` into `detect` with the "Undetermined handling" layer |
| S3 status (2026-11-18) | done: ruling 12 — `$HOME` / `${HOME}` / leading `~` are the hook environment's `HOME`, in the evaluator (`eval.rs` `Env::initial`) and in the one static resolver every legacy reading sees (`readings/static_bindings.rs`), so a `$HOME` spelling is judged exactly as its absolute spelling. NOT done (at S3; `judge` is wired for B.1 since S3b, `classify` is still the stub): `judge` / `classify` are still the S1 stubs and are not wired (open question raised to the user: under raise-only every Known Allow cell is inert, so wiring only adds Asks — including `Undetermined` for every unmodelled option such as `cp -p` — whose effect on commands allowed today is not measured by the block-log replay) |
| S3b `tableb-judge` (user ruling 2026-11-18) | done: `judge` wired into the final decision (`ast/mod.rs::table_b_verdict`, folded raise-only in `detect/readings.rs::detect_bash_every_reading`) ONLY for commands with a **B.1** row (`ast/judge/table_b.rs`). For such a command an option not in its row, an undetermined word that may be an option, or a `dd` operand that is not `key=value` → `Undetermined` → Ask (main) / Deny (subagent); a target word whose value the evaluator cannot determine (unquoted glob, `~user`, `$( )`, unbound / positional parameter, relative path after an undetermined `cd`, `xargs` operand read from non-literal stdin) → `Unresolved` → Ask / Deny. Commands without a B.1 row, and `analyze` = `Undetermined`, are unchanged; unknown programs stay Allow; the legacy layer is not lowered (a legacy Deny / Ask, with its reason, wins). See "S3b notes" |
| S4 `stdin-program` | Table B B.5 rows for shells: `-c` inline, stdin (`StdinSource::Literal` bodies), `eval`; the script-file row and the direct-execution row (Undetermined, and the recursive judgement of a body the same command writes — rulings 10, 17); wrapper → interpreter unwrap. `source` / `.` are NOT in S4 (ruling 16) |
| S5 `python-parser` | Table B B.5 python row (a real Python parser; effects × targets of the parsed program judged by Table C); legacy python readings until then (ruling 15) |
| S6 `retire-readings` | remove the string-reading fallbacks of every migrated row (`targets.rs::resolve` callers, `readings.rs` re-readings); until S6 the whole legacy layer is raise-only (ruling 14), so Table C Allow cells a legacy reading answers Ask / Deny today turn GREEN here; revisit the deferred Open questions 1–4 |
| quarantine plugin (separate) | `source` / `.` → Ask (main) / Deny (subagent), introduced together with the script-quarantine plugin (ruling 16) |

### S3b notes (2026-11-18)

* NOT wired in S3b (verdict unchanged, legacy only): B.2 redirects, B.3
  wrappers (`sudo rm …`, `command rm …`), B.4 git, B.5 interpreters, the
  "undetermined command word → Undetermined" rule, and every Table C cell of
  a target whose value IS determined — `classify` is still the S1 stub, so a
  placed target gets no finding and the raise-only legacy layer alone judges
  it (S3b `Judgement::Allow` = "nothing undetermined found").
* Option lists: the B.1 rows above, plus the GNU long spelling of a listed
  short option (`--append` = `-a`, `--target-directory=DIR` = `-t DIR`, …;
  list in `table_b.rs`). Options after operands count as options (GNU
  permutation), except for `xargs`. An undetermined word counts as a possible
  option unless its raw text provably starts with a literal that is not `-`
  (`"/tmp/$x"`, `~user/f`). `chmod -w f`: a `-[rwxXst]+` word is the mode.
  `sed` has a row only when a literal `-…i…` / `--in-place` word is present;
  BSD `-i ''` is modelled.
* **Implementer additions, pending the user's confirmation** (each keeps a
  previously-passing test green — CA-blastguard-011 / -015 in
  `detect/tests.rs`: `rm --help`, `rm -h`, `tee --help 2>&1`,
  `truncate --help` → Allow):
  1. `--help` / `--version` are accepted by every B.1 row;
  2. an unlisted option is NOT Undetermined when every word after the
     command is a literal option without an argument (`rm -h`): no word is
     left for the option to turn into, or away from, a target.
* `find`: an undetermined word in the expression is not read (it may be the
  argument of a primary; primary arities are not modelled), so `find . "$x"`
  where `$x` is `-delete` is NOT caught by S3b (legacy verdict unchanged).
* `rmdir -p` ancestors are not extracted (they only matter for a placed path).

## Open questions

Decided by the follow-up rulings 8–10 (2026-11-17): scratchpad scope →
`Scratch` row; own-worktree scope → `OwnWorktree` / `OtherWorktree` rows;
`bash script.sh` → B.5 script-file row. Unresolved targets — non-delete writes
(`echo x > "$f"`) and deletes alike — are Ask / Deny per ruling 2 as Table C
states (accepted 2026-11-17; where today's rule answers Deny, the raise-only
layer keeps that Deny).

Deferred (ruling 11): **legacy (today's behaviour) until S6, revisited there.**
S3 does not implement these.

1. **`find -exec … {}`**: may `{}` inherit the search root's class
   (descendants), or is it always Unresolved?
2. **sed scripts that write / execute** (`w`, `W`, `e`, `s///w`, `s///e`): needs
   a sed-script tokenizer — inside the "no corner-case modelling" ruling or not?
3. **Project / Other / OtherWorktree cells marked legacy**: should Table C state
   explicit verdicts (e.g. Truncate of an Other path = Allow) instead of
   reproducing the current rules?
4. **Non-path detectors** (git push / branch -D, gh, high_blast tiers,
   osascript, …): stay as legacy detectors beside `judge`, or migrate into
   Table B?
